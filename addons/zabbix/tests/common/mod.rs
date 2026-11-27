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
use hd_zabbix::sync::Sync;
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
    http: reqwest::Client,
    cookie: String,
    process: Child,
    database: String,
    dir: PathBuf,
}

impl Core {
    pub async fn start() -> Core {
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
            "http://127.0.0.1:9",
            "--scope",
            "directory:read",
            "--scope",
            "catalog:write",
            "--scope",
            "names:write",
            "--scope",
            "conflicts:write",
        ]);
        let token = added
            .lines()
            .find_map(|l| l.strip_prefix("token: "))
            .expect("hd addon add prints the token")
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

    pub async fn delete_host(&mut self, host: &str) {
        let _: Value = self.admin.call("host.delete", json!([host])).await.unwrap();
        self.hosts.retain(|h| h != host);
    }

    /// The add-on, between this Zabbix with the test's API token and `core`.
    pub fn addon(&self, core: &Core, kinds: Kinds) -> Sync {
        Sync {
            core: core.addon(),
            zabbix: Zabbix::new(&zabbix_url(), &self.api_token, Duration::from_secs(30)).unwrap(),
            system: "Zabbix".into(),
            kinds,
        }
    }

    /// Removes what the test created in Zabbix.
    pub async fn clean_up(self) {
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
