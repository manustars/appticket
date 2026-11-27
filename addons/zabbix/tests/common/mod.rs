//! The real core and the Zabbix in containers, for the tests of the add-on against both.
//!
//! The core is the `hd` binary of this workspace (`cargo build -p hd-core`, or the one
//! `HD_TEST_CORE_BIN` names), run as an install runs it, on a database of its own made on the
//! test server of `DATABASE_URL` and dropped afterwards. Zabbix is the one at
//! `HD_TEST_ZABBIX_URL` (by default the one `compose.yaml` starts, on 127.0.0.1:55180), signed in
//! as the default `Admin` of a fresh container: each test makes its own API token, and host
//! groups and hosts named after a prefix of its own, which it removes at the end.

#![allow(dead_code)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hd_addon_sdk::Client;
use hd_zabbix::kinds::Kinds;
use hd_zabbix::sync::Syncer;
use hd_zabbix::zabbix::Zabbix;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, Connection, PgConnection};
use uuid::Uuid;

/// The address of the Zabbix the tests use.
pub fn zabbix_url() -> String {
    std::env::var("HD_TEST_ZABBIX_URL").unwrap_or_else(|_| "http://127.0.0.1:55180".into())
}

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .or_else(|_| dotenvy::var("DATABASE_URL"))
        .expect("DATABASE_URL is set for the tests")
}

/// The `hd` binary of this workspace, next to the test's own directory.
fn hd_binary() -> PathBuf {
    if let Ok(path) = std::env::var("HD_TEST_CORE_BIN") {
        return PathBuf::from(path);
    }
    // The test runs from somewhere below target/<profile>, where cargo puts `hd`.
    let exe = std::env::current_exe().unwrap();
    let name = format!("hd{}", std::env::consts::EXE_SUFFIX);
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join(&name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "no {name} above {}: build the core first with `cargo build -p hd-core`, or \
                 name it in HD_TEST_CORE_BIN",
                exe.display()
            )
        })
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

const AGENT: &str = "ada.admin@example.com";
const PASSWORD: &str = "zabbix-test-password";
/// The add-on's name on the core.
pub const ADDON: &str = "zabbix";

/// The core, running with a database of its own, an administrator signed in and the add-on
/// registered; both stopped and dropped with it.
pub struct Core {
    pub url: String,
    /// The add-on's token.
    pub token: String,
    /// The add-on's event secret, which the core signs its reads of live details with.
    pub event_secret: String,
    http: reqwest::Client,
    cookie: String,
    process: Child,
    database: String,
    dir: PathBuf,
}

impl Core {
    pub async fn start() -> Core {
        Core::start_at("http://127.0.0.1:9").await
    }

    /// The core, with the add-on registered at `addon_url`, where it asks for live details.
    pub async fn start_at(addon_url: &str) -> Core {
        let database = format!("hd_zabbix_{}", Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&database_url()).await.unwrap();
        sqlx::raw_sql(AssertSqlSafe(format!(r#"CREATE DATABASE "{database}""#)))
            .execute(&mut admin)
            .await
            .unwrap();
        let mut url = url::Url::parse(&database_url()).unwrap();
        url.set_path(&database);
        let database_url = url.to_string();
        let dir = std::env::temp_dir().join(format!("hd-zabbix-core-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let port = free_port();
        let hd = |args: &[&str]| {
            let mut command = Command::new(hd_binary());
            command.args(args).current_dir(&dir).env_clear();
            for name in ["PATH", "SystemRoot", "SYSTEMROOT", "TEMP", "TMP"] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            command
                .env("HD_DATABASE_URL", &database_url)
                .env("HD_LISTEN", format!("127.0.0.1:{port}"))
                .env("HD_AGENT_PASSWORD", PASSWORD)
                .env("HD_LOG", "warn");
            command
        };
        let run = |args: &[&str]| {
            let output = hd(args).output().unwrap();
            assert!(
                output.status.success(),
                "hd {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        run(&["migrate"]);
        run(&["agent", "add", AGENT, "--name", "Ada Admin"]);
        let added = run(&[
            "addon",
            "add",
            ADDON,
            "--url",
            addon_url,
            "--scope",
            "directory:read",
            "--scope",
            "catalog:write",
            "--scope",
            "names:write",
            "--scope",
            "conflicts:write",
            "--scope",
            "tickets:read",
            "--scope",
            "tickets:write",
        ]);
        let token = added
            .lines()
            .find_map(|l| l.strip_prefix("token: "))
            .expect("hd addon add prints the token")
            .trim()
            .to_owned();
        let event_secret = added
            .lines()
            .find_map(|l| l.strip_prefix("event secret: "))
            .expect("hd addon add prints the event secret")
            .trim()
            .to_owned();
        let http = hd_addon_sdk::http_client(Duration::from_secs(30)).unwrap();
        // Its log in a file, not in the test's output, whose pipe it would hold open.
        let log = dir.join("core.log");
        let process = hd(&["serve"])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let mut core = Core {
            url: format!("http://127.0.0.1:{port}"),
            token,
            event_secret,
            http,
            cookie: String::new(),
            process,
            database,
            dir,
        };
        let started = Instant::now();
        loop {
            if let Ok(r) = core
                .http
                .get(format!("{}/api/health", core.url))
                .send()
                .await
                && r.status().is_success()
            {
                break;
            }
            assert!(
                core.process.try_wait().unwrap().is_none(),
                "the core stopped at startup: {}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the core does not answer"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let signed_in = core
            .http
            .post(format!("{}/api/session", core.url))
            .json(&json!({"email": AGENT, "password": PASSWORD}))
            .send()
            .await
            .unwrap();
        assert!(signed_in.status().is_success(), "signing in");
        // The session cookie is Secure: the tests send it back themselves over plain HTTP.
        core.cookie = signed_in
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("hd_session="))
            .and_then(|v| v.split(';').next())
            .expect("a session cookie")
            .to_owned();
        core
    }

    /// The add-on's client of the core's API.
    pub fn addon(&self) -> Client {
        Client::new(&self.url, &self.token).unwrap()
    }

    /// Calls the agents' API as the administrator.
    pub async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.url))
            .header(reqwest::header::COOKIE, &self.cookie);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn get(&self, path: &str) -> Value {
        let (status, json) = self.call("GET", path, None).await;
        assert_eq!(status, StatusCode::OK, "GET {path}: {json}");
        json
    }

    /// POSTs `body` and answers the id of what it created.
    pub async fn create(&self, path: &str, body: Value) -> Uuid {
        let (status, json) = self.call("POST", path, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "POST {path}: {json}");
        json["id"].as_str().unwrap().parse().unwrap()
    }

    pub async fn customer(&self, name: &str) -> Uuid {
        self.create("/api/customers/all", json!({"name": name}))
            .await
    }

    pub async fn asset_kind(&self, name: &str) -> Uuid {
        self.create(
            "/api/asset-kinds",
            json!({"name": name, "moves_with_holder": false}),
        )
        .await
    }

    /// Gives the customer an external reference in the add-on's system; answers its id.
    pub async fn reference(&self, customer: Uuid, name: &str) -> Uuid {
        self.create(
            &format!("/api/customers/{customer}/external-references"),
            json!({"system": "Zabbix", "name": name}),
        )
        .await
    }

    /// The customer's assets, retired ones included, as `(name, kind, serial number, source,
    /// retired)`, by name.
    pub async fn assets(
        &self,
        customer: Uuid,
    ) -> Vec<(String, String, Option<String>, Option<String>, bool)> {
        let assets = self.get(&format!("/api/customers/{customer}/assets")).await;
        let mut listed: Vec<_> = assets
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["name"].as_str().unwrap().to_owned(),
                    a["kind"].as_str().unwrap().to_owned(),
                    a["serial_number"].as_str().map(str::to_owned),
                    a["source_addon"].as_str().map(str::to_owned),
                    !a["retired_at"].is_null(),
                )
            })
            .collect();
        listed.sort();
        listed
    }

    /// A service the customer uses; answers its id.
    pub async fn service(&self, name: &str, customer: Uuid) -> Uuid {
        let service = self.create("/api/services", json!({"name": name})).await;
        let path = format!("/api/services/{service}/customers/{customer}");
        let (status, json) = self.call("PUT", &path, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "PUT {path}: {json}");
        service
    }

    /// A ticket as agents read it.
    pub async fn ticket(&self, number: i64) -> Value {
        self.get(&format!("/api/tickets/{number}")).await
    }

    /// The numbers of every ticket, as agents list them.
    pub async fn tickets(&self) -> Vec<i64> {
        let mut numbers: Vec<i64> = self
            .get("/api/tickets")
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["number"].as_i64().unwrap())
            .collect();
        numbers.sort();
        numbers
    }

    /// The bodies of the ticket's notes by the add-on, oldest first.
    pub async fn addon_notes(&self, number: i64) -> Vec<String> {
        self.ticket(number).await["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "note" && e["by_addon"] == true)
            .map(|e| e["body"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The administrator writes a note on the ticket: an agent has worked on it.
    pub async fn work_on(&self, number: i64) {
        let path = format!("/api/tickets/{number}/notes");
        let (status, json) = self
            .call("POST", &path, Some(json!({"body": "Looking into it."})))
            .await;
        assert_eq!(status, StatusCode::CREATED, "POST {path}: {json}");
    }

    /// The add-on's state, as those who manage add-ons read it.
    pub async fn state(&self) -> Value {
        let addons = self.get("/api/admin/addons").await;
        addons
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == ADDON)
            .unwrap_or_else(|| panic!("no add-on {ADDON} in {addons}"))
            .clone()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let database = self.database.clone();
        // Not on the test's runtime, which may be the one dropping this.
        let dropped = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let mut admin = PgConnection::connect(&database_url()).await?;
                    sqlx::raw_sql(AssertSqlSafe(format!(
                        r#"DROP DATABASE IF EXISTS "{database}" WITH (FORCE)"#
                    )))
                    .execute(&mut admin)
                    .await
                    .map(|_| ())
                })
        })
        .join();
        if !matches!(dropped, Ok(Ok(()))) {
            eprintln!("dropping {} failed", self.database);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Zabbix's API as its administrator. Zabbix can fail one of two writes made at the same time
/// ("Database error occurred"), so the tests' calls take turns.
pub struct Admin(Zabbix);

static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Admin {
    pub async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> anyhow::Result<T> {
        let _turn = TURN.lock().await;
        self.0.call(method, params).await
    }
}

/// The Zabbix of the tests, signed in as its default administrator, with a prefix of the test's
/// own for what it creates there.
pub struct TestZabbix {
    pub admin: Admin,
    /// An API token made for the test, which the add-on uses.
    pub api_token: String,
    token_id: String,
    pub prefix: String,
    groups: Vec<String>,
    hosts: Vec<String>,
    templates: Vec<String>,
    users: Vec<String>,
    actions: Vec<String>,
    media_types: Vec<String>,
}

impl TestZabbix {
    pub async fn connect() -> TestZabbix {
        let url = zabbix_url();
        let anonymous = Zabbix::new(&url, "", Duration::from_secs(30)).unwrap();
        let version = anonymous
            .version()
            .await
            .unwrap_or_else(|e| panic!("no Zabbix at {url} (HD_TEST_ZABBIX_URL): {e:#}"));
        eprintln!("Zabbix {version} at {url}");
        let session: String = Admin(anonymous)
            .call(
                "user.login",
                json!({"username": "Admin", "password": "zabbix"}),
            )
            .await
            .unwrap();
        let admin = Admin(Zabbix::new(&url, &session, Duration::from_secs(30)).unwrap());
        let prefix = format!("T{}", &Uuid::new_v4().simple().to_string()[..8]);
        let users: Vec<Value> = admin
            .call(
                "user.get",
                json!({"output": ["userid"], "filter": {"username": "Admin"}}),
            )
            .await
            .unwrap();
        let created: Value = admin
            .call(
                "token.create",
                json!({"name": format!("{prefix} add-on test"),
                       "userid": users[0]["userid"]}),
            )
            .await
            .unwrap();
        let token_id = created["tokenids"][0].as_str().unwrap().to_owned();
        let generated: Vec<Value> = admin
            .call("token.generate", json!([token_id]))
            .await
            .unwrap();
        TestZabbix {
            admin,
            api_token: generated[0]["token"].as_str().unwrap().to_owned(),
            token_id,
            prefix,
            groups: vec![],
            hosts: vec![],
            templates: vec![],
            users: vec![],
            actions: vec![],
            media_types: vec![],
        }
    }

    /// `name` with the test's prefix.
    pub fn name(&self, name: &str) -> String {
        format!("{} {name}", self.prefix)
    }

    /// A host group named `name` after the prefix, such as `T1a2b3c4d Brightfield/Offices`.
    pub async fn group(&mut self, name: &str) -> String {
        let created: Value = self
            .admin
            .call("hostgroup.create", json!({"name": self.name(name)}))
            .await
            .unwrap();
        let id = created["groupids"][0].as_str().unwrap().to_owned();
        self.groups.push(id.clone());
        id
    }

    /// A template named `name` after the prefix.
    pub async fn template(&mut self, name: &str) -> String {
        let groups: Vec<Value> = self
            .admin
            .call(
                "templategroup.get",
                json!({"output": ["groupid"], "filter": {"name": ["Templates"]}}),
            )
            .await
            .unwrap();
        let created: Value = self
            .admin
            .call(
                "template.create",
                json!({"host": self.name(name),
                       "groups": [{"groupid": groups[0]["groupid"]}]}),
            )
            .await
            .unwrap();
        let id = created["templateids"][0].as_str().unwrap().to_owned();
        self.templates.push(id.clone());
        id
    }

    /// A host with the technical name `host` after the prefix and the fields given (such as
    /// `name`, `templates`, `tags`, `inventory_mode` and `inventory`), in the groups with these
    /// ids.
    pub async fn host(&mut self, host: &str, groups: &[&str], fields: Value) -> String {
        let mut params = json!({
            "host": format!("{}-{host}", self.prefix),
            "groups": groups.iter().map(|g| json!({"groupid": g})).collect::<Vec<_>>(),
        });
        params
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let created: Value = self.admin.call("host.create", params).await.unwrap();
        let id = created["hostids"][0].as_str().unwrap().to_owned();
        self.hosts.push(id.clone());
        id
    }

    /// Puts the host in these groups only.
    pub async fn move_host(&self, host: &str, groups: &[&str]) {
        let _: Value = self
            .admin
            .call(
                "host.update",
                json!({"hostid": host,
                       "groups": groups.iter().map(|g| json!({"groupid": g})).collect::<Vec<_>>()}),
            )
            .await
            .unwrap();
    }

    /// Changes the fields given of the host, such as `name`, `templates`, `tags` or `inventory`.
    pub async fn update_host(&self, host: &str, fields: Value) {
        let mut params = json!({"hostid": host});
        params
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let _: Value = self.admin.call("host.update", params).await.unwrap();
    }

    pub async fn delete_host(&mut self, host: &str) {
        let _: Value = self.admin.call("host.delete", json!([host])).await.unwrap();
        self.hosts.retain(|h| h != host);
    }

    /// The add-on, between this Zabbix with the test's API token and `core`.
    pub fn addon(&self, core: &Core, kinds: Kinds) -> Syncer {
        Syncer::new(
            core.addon(),
            Zabbix::new(&zabbix_url(), &self.api_token, Duration::from_secs(30)).unwrap(),
            "Zabbix",
            kinds,
        )
    }

    /// Removes what the test created in Zabbix.
    pub async fn clean_up(self) {
        if !self.actions.is_empty() {
            let _: Value = self
                .admin
                .call("action.delete", json!(self.actions))
                .await
                .unwrap();
        }
        if !self.users.is_empty() {
            let _: Value = self
                .admin
                .call("user.delete", json!(self.users))
                .await
                .unwrap();
        }
        if !self.media_types.is_empty() {
            let _: Value = self
                .admin
                .call("mediatype.delete", json!(self.media_types))
                .await
                .unwrap();
        }
        if !self.hosts.is_empty() {
            let _: Value = self
                .admin
                .call("host.delete", json!(self.hosts))
                .await
                .unwrap();
        }
        if !self.templates.is_empty() {
            let _: Value = self
                .admin
                .call("template.delete", json!(self.templates))
                .await
                .unwrap();
        }
        if !self.groups.is_empty() {
            let _: Value = self
                .admin
                .call("hostgroup.delete", json!(self.groups))
                .await
                .unwrap();
        }
        let _: Value = self
            .admin
            .call("token.delete", json!([self.token_id]))
            .await
            .unwrap();
    }
}

/// What the tests of live details make in Zabbix: items, triggers and the values that raise
/// problems.
impl TestZabbix {
    /// The add-on's live details from this Zabbix, linking to its frontend.
    pub fn details(&self) -> hd_zabbix::details::Details {
        hd_zabbix::details::Details::new(
            Zabbix::new(&zabbix_url(), &self.api_token, hd_zabbix::details::DEADLINE).unwrap(),
            &zabbix_url(),
        )
    }

    /// The technical name of the test's host `host`.
    pub fn host_name(&self, host: &str) -> String {
        format!("{}-{host}", self.prefix)
    }

    /// An item of the host with these fields, such as `key_`, `type` and `value_type`.
    pub async fn item(&self, host: &str, name: &str, fields: Value) -> String {
        let mut params = json!({"hostid": host, "name": name});
        params
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let created: Value = self.admin.call("item.create", params).await.unwrap();
        created["itemids"][0].as_str().unwrap().to_owned()
    }

    /// A trigger with this expression and severity, from 0 (not classified) to 5 (disaster).
    pub async fn trigger(&self, description: &str, expression: &str, severity: u8) {
        let _: Value = self
            .admin
            .call(
                "trigger.create",
                json!({"description": description, "expression": expression,
                       "priority": severity}),
            )
            .await
            .unwrap();
    }

    /// Sends `value` to the trapper item with `history.push`, again until the server takes it:
    /// it learns of new items from its database every few seconds.
    pub async fn push(&self, item: &str, value: &str) {
        let started = Instant::now();
        loop {
            let pushed: Value = self
                .admin
                .call("history.push", json!([{"itemid": item, "value": value}]))
                .await
                .unwrap();
            if pushed["data"][0].get("error").is_none() {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "the server does not take values for item {item}: {pushed}"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Waits until `until` holds of the host's open problems, as `problem.get` answers them.
    pub async fn wait_for_problems(&self, host: &str, until: impl Fn(&[Value]) -> bool) {
        let started = Instant::now();
        loop {
            let problems: Vec<Value> = self
                .admin
                .call(
                    "problem.get",
                    json!({"output": ["eventid", "severity"], "hostids": [host]}),
                )
                .await
                .unwrap();
            if until(&problems) {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "the problems of host {host} are {problems:?}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// The `available` of each of the host's interfaces.
    pub async fn availability(&self, host: &str) -> Vec<String> {
        let hosts: Vec<Value> = self
            .admin
            .call(
                "host.get",
                json!({"output": ["hostid"], "hostids": [host],
                       "selectInterfaces": ["available"]}),
            )
            .await
            .unwrap();
        hosts[0]["interfaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["available"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// A port of the test's own for the add-on's live details, and its URL, to register the add-on
/// at before it serves them there.
pub async fn listener() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}

/// Serves the add-on's live details on `listener`, as the add-on does, checking the core's
/// signature with `event_secret`.
pub fn serve(
    listener: tokio::net::TcpListener,
    details: hd_zabbix::details::Details,
    event_secret: &str,
) {
    let app = hd_zabbix::details::router(
        std::sync::Arc::new(details),
        hd_addon_sdk::Verifier::new(event_secret),
    );
    tokio::spawn(async move { axum::serve(listener, app).await });
}

/// Live details as the agents' page reads them: the state, the fields as `(English label,
/// Italian label, value)` and the links as `(English label, URL)`.
pub type Read = (String, Vec<(String, String, String)>, Vec<(String, String)>);

/// The add-on's live details of each of the customer's assets as the agents' page reads them,
/// `None` when they are unavailable.
pub async fn live_details(core: &Core, customer: Uuid) -> Vec<Option<Read>> {
    let read = core
        .get(&format!("/api/customers/{customer}/assets/live-details"))
        .await;
    let label = |v: &Value, lang: &str| v["label"][lang].as_str().unwrap().to_owned();
    read.as_array()
        .unwrap()
        .iter()
        .filter(|d| d["addon"] == ADDON)
        .map(|d| {
            let details = &d["details"];
            if details.is_null() {
                return None;
            }
            Some((
                details["state"].as_str().unwrap().to_owned(),
                details["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| {
                        (
                            label(f, "en"),
                            label(f, "it"),
                            f["value"].as_str().unwrap().to_owned(),
                        )
                    })
                    .collect(),
                details["links"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| (label(l, "en"), l["url"].as_str().unwrap().to_owned()))
                    .collect(),
            ))
        })
        .collect()
}

/// What the tests of alerts make in Zabbix: triggers whose problems they raise and recover,
/// users, and the media type and action that send the problems to the add-on's webhook.
impl TestZabbix {
    /// A trigger with this expression and severity and the fields given, such as `tags` or
    /// `opdata`; answers its id.
    pub async fn trigger_with(
        &self,
        description: &str,
        expression: &str,
        severity: u8,
        fields: Value,
    ) -> String {
        let mut params = json!({"description": description, "expression": expression,
                                "priority": severity});
        params
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let created: Value = self.admin.call("trigger.create", params).await.unwrap();
        created["triggerids"][0].as_str().unwrap().to_owned()
    }

    /// Pushes `value` to the trapper item until the trigger has a problem, and answers the id of
    /// its event: the server takes values for an item once it has read the new configuration.
    pub async fn raise(&self, item: &str, trigger: &str, value: &str) -> String {
        let started = Instant::now();
        loop {
            let pushed: Value = self
                .admin
                .call("history.push", json!([{"itemid": item, "value": value}]))
                .await
                .unwrap();
            let events: Vec<Value> = self
                .admin
                .call(
                    "event.get",
                    json!({"objectids": [trigger], "value": 1, "output": ["eventid"]}),
                )
                .await
                .unwrap();
            if let Some(event) = events.first() {
                return event["eventid"].as_str().unwrap().to_owned();
            }
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "no problem of trigger {trigger} after pushing values: {pushed}"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Pushes `value` to the trapper item until the problem of `event` has recovered; answers
    /// the recovery's event id.
    pub async fn recover(&self, item: &str, event: &str, value: &str) -> String {
        self.push(item, value).await;
        let started = Instant::now();
        loop {
            let events: Vec<Value> = self
                .admin
                .call(
                    "event.get",
                    json!({"eventids": [event], "output": ["r_eventid"]}),
                )
                .await
                .unwrap();
            let recovery = events[0]["r_eventid"].as_str().unwrap_or("0");
            if recovery != "0" {
                return recovery.to_owned();
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "problem {event} does not recover"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// The tags of the event, as name and value.
    pub async fn event_tags(&self, event: &str) -> Vec<(String, String)> {
        let events: Vec<Value> = self
            .admin
            .call(
                "event.get",
                json!({"eventids": [event], "output": ["eventid"],
                       "selectTags": ["tag", "value"]}),
            )
            .await
            .unwrap();
        events[0]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["tag"].as_str().unwrap().to_owned(),
                    t["value"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// A Zabbix user of the test's own, a Super admin, named `username` after the prefix, with
    /// the fields given, such as `name`, `surname` and `medias`; answers its username.
    pub async fn user(&mut self, username: &str, fields: Value) -> String {
        let username = format!("{}-{username}", self.prefix);
        let groups: Vec<Value> = self
            .admin
            .call(
                "usergroup.get",
                json!({"output": ["usrgrpid"], "filter": {"name": ["Zabbix administrators"]}}),
            )
            .await
            .unwrap();
        let mut params = json!({"username": username, "passwd": USER_PASSWORD,
                                "roleid": SUPER_ADMIN_ROLE,
                                "usrgrps": [{"usrgrpid": groups[0]["usrgrpid"]}]});
        params
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let created: Value = self.admin.call("user.create", params).await.unwrap();
        self.users
            .push(created["userids"][0].as_str().unwrap().to_owned());
        username
    }

    /// Zabbix's API as a user the test made.
    pub async fn sign_in(&self, username: &str) -> Admin {
        let anonymous = Zabbix::new(&zabbix_url(), "", Duration::from_secs(30)).unwrap();
        let session: String = Admin(anonymous)
            .call(
                "user.login",
                json!({"username": username, "password": USER_PASSWORD}),
            )
            .await
            .unwrap();
        Admin(Zabbix::new(&zabbix_url(), &session, Duration::from_secs(30)).unwrap())
    }

    /// Imports the add-on's media type (`zabbix-media-type.yaml`) under a name of the test's own,
    /// and sets it to send to `url` with `secret`; answers its id.
    pub async fn media_type(&mut self, url: &str, secret: &str) -> String {
        let shipped = include_str!("../../zabbix-media-type.yaml");
        let name = self.name("Help desk");
        let file = shipped.replace("- name: 'Help desk'", &format!("- name: '{name}'"));
        assert_ne!(file, shipped, "the media type is named Help desk");
        let _: Value = self
            .admin
            .call(
                "configuration.import",
                json!({"format": "yaml", "source": file,
                       "rules": {"mediaTypes": {"createMissing": true}}}),
            )
            .await
            .unwrap();
        let found: Vec<Value> = self
            .admin
            .call(
                "mediatype.get",
                json!({"output": ["mediatypeid", "status", "process_tags", "parameters"],
                       "filter": {"name": [name]}}),
            )
            .await
            .unwrap();
        let media_type = &found[0];
        let id = media_type["mediatypeid"].as_str().unwrap().to_owned();
        self.media_types.push(id.clone());
        assert_eq!(media_type["status"], "0", "imported enabled");
        assert_eq!(media_type["process_tags"], "1", "imported processing tags");
        let parameters: Vec<Value> = media_type["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| match p["name"].as_str().unwrap() {
                "url" => json!({"name": "url", "value": url}),
                "secret" => json!({"name": "secret", "value": secret}),
                _ => p.clone(),
            })
            .collect();
        let _: Value = self
            .admin
            .call(
                "mediatype.update",
                json!({"mediatypeid": id, "parameters": parameters}),
            )
            .await
            .unwrap();
        id
    }

    /// An action that sends the problems of the hosts of `group` to `user` through `media_type`,
    /// and their recoveries and updates to everyone they went to.
    pub async fn action(&mut self, group: &str, media_type: &str, user: &str) -> String {
        let users: Vec<Value> = self
            .admin
            .call(
                "user.get",
                json!({"output": ["userid"], "filter": {"username": [user]}}),
            )
            .await
            .unwrap();
        let created: Value = self
            .admin
            .call(
                "action.create",
                json!({
                    "name": self.name("Help desk"),
                    "eventsource": 0,
                    "status": 0,
                    "esc_period": "1h",
                    // Host group is condition type 0, "equals" operator 0.
                    "filter": {"evaltype": 0,
                               "conditions": [{"conditiontype": 0, "operator": 0,
                                               "value": group}]},
                    "operations": [{"operationtype": 0,
                                    "opmessage": {"default_msg": 1, "mediatypeid": media_type},
                                    "opmessage_usr": [{"userid": users[0]["userid"]}]}],
                    // Notify all involved: 11 on recovery, 12 on update.
                    "recovery_operations": [{"operationtype": 11,
                                             "opmessage": {"default_msg": 1}}],
                    "update_operations": [{"operationtype": 12,
                                           "opmessage": {"default_msg": 1}}],
                }),
            )
            .await
            .unwrap();
        let id = created["actionids"][0].as_str().unwrap().to_owned();
        self.actions.push(id.clone());
        id
    }

    /// What Zabbix's alerts of the event say, for a test that waits in vain for the webhook.
    pub async fn alerts_of(&self, event: &str) -> Value {
        self.admin
            .call(
                "alert.get",
                json!({"eventids": [event], "output": ["status", "error", "retries"]}),
            )
            .await
            .unwrap()
    }

    /// The add-on's alerts, between this Zabbix with the test's API token and `core`.
    pub fn alerts(
        &self,
        core: &Core,
        rules: hd_zabbix::rules::Rules,
    ) -> hd_zabbix::webhook::Alerts {
        hd_zabbix::webhook::Alerts::new(
            core.addon(),
            Zabbix::new(&zabbix_url(), &self.api_token, hd_zabbix::webhook::DEADLINE).unwrap(),
            "Zabbix",
            rules,
        )
    }
}

/// The password of the Zabbix users the tests make: none of their names is in it.
const USER_PASSWORD: &str = "Kx7-test-pass-31!q";

/// Zabbix's role `Super admin role`.
const SUPER_ADMIN_ROLE: &str = "3";

/// The rules of `[[alerts.rules]]` in this TOML.
pub fn rules(toml_text: &str) -> hd_zabbix::rules::Rules {
    let table: toml::Table = toml::from_str(toml_text).unwrap();
    let rules = table["alerts"]["rules"].as_array().unwrap().clone();
    hd_zabbix::rules::Rules::read(rules).unwrap()
}

/// A port for the add-on's webhook, bound on `HD_TEST_WEBHOOK_LISTEN` (default `0.0.0.0:0`, so
/// that the Zabbix server's container reaches it), with the URL the test posts to and the URL
/// Zabbix's server reaches it at, on the host `HD_TEST_ZABBIX_WEBHOOK_HOST` names (default
/// `host.docker.internal`, which `compose.yaml` maps to the host).
pub async fn webhook_listener() -> (tokio::net::TcpListener, String, String) {
    let listen = std::env::var("HD_TEST_WEBHOOK_LISTEN").unwrap_or_else(|_| "0.0.0.0:0".into());
    let listener = tokio::net::TcpListener::bind(&listen).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = std::env::var("HD_TEST_ZABBIX_WEBHOOK_HOST")
        .unwrap_or_else(|_| "host.docker.internal".into());
    let path = hd_zabbix::webhook::PATH;
    (
        listener,
        format!("http://127.0.0.1:{port}{path}"),
        format!("http://{host}:{port}{path}"),
    )
}

/// Serves the add-on's webhook on `listener`, as the add-on does, with `secret`.
pub fn serve_webhook(
    listener: tokio::net::TcpListener,
    alerts: hd_zabbix::webhook::Alerts,
    secret: &str,
) {
    let app = hd_zabbix::webhook::router(std::sync::Arc::new(alerts), secret.to_owned());
    tokio::spawn(async move { axum::serve(listener, app).await });
}

/// Posts what Zabbix's media type sends to the webhook at `url`, with `secret`; answers the
/// status and the body.
pub async fn post_event(url: &str, secret: &str, event: Value) -> (StatusCode, Value) {
    let response = hd_addon_sdk::http_client(Duration::from_secs(60))
        .unwrap()
        .post(url)
        .bearer_auth(secret)
        .json(&event)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

/// What the media type sends for a problem, as Zabbix 7.0 resolves its parameters: those of
/// updates stay as they are written.
pub fn problem_sent(event: &str) -> Value {
    json!({"event_id": event, "event_value": "1", "event_update_status": "0",
           "event_source": "0", "user": "{USER.FULLNAME}",
           "action": "{EVENT.UPDATE.ACTION}", "message": "{EVENT.UPDATE.MESSAGE}"})
}

/// What the media type sends for the recovery of the problem of `event`.
pub fn recovery_sent(event: &str) -> Value {
    json!({"event_id": event, "event_value": "0", "event_update_status": "0",
           "event_source": "0", "user": "{USER.FULLNAME}",
           "action": "{EVENT.UPDATE.ACTION}", "message": "{EVENT.UPDATE.MESSAGE}"})
}

/// What the media type sends for an update of the problem of `event` by `user`, as
/// `{USER.FULLNAME}` names them.
pub fn update_sent(event: &str, user: &str, action: &str, message: &str) -> Value {
    json!({"event_id": event, "event_value": "1", "event_update_status": "1",
           "event_source": "0", "user": user, "action": action, "message": message})
}
