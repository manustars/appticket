//! Importing customers, their sites, their requesters and their assets from CSV files. Rules are
//! in docs/domain-rules.md ("Importing").

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::{Acquire, PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::customers::CustomerDetails;
use crate::error::{Error, Result, rule};
use crate::requesters::RequesterDetails;
use crate::roles::{Grant, Permission};

/// What a file of an import holds, one kind per file. Files are taken in this order, so that a
/// row can refer to something an earlier file creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportKind)]
pub enum Kind {
    Customers,
    Sites,
    Requesters,
    Assets,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Customers => "customers",
            Kind::Sites => "sites",
            Kind::Requesters => "requesters",
            Kind::Assets => "assets",
        }
    }
}

/// The column the rows left out come back with, saying why; an import ignores it, so that the
/// file can be corrected and imported again.
const ERROR_COLUMN: &str = "error";

/// One CSV file of an import.
pub struct File {
    pub kind: Kind,
    pub content: Vec<u8>,
}

/// Whether an import only shows what it would do, or does it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Preview,
    Apply,
}

/// What an import does, or would do, file by file.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ImportReport)]
pub struct Report {
    /// Whether the import was applied; a preview changes nothing.
    pub applied: bool,
    pub files: Vec<FileReport>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ImportFileReport)]
pub struct FileReport {
    pub kind: Kind,
    /// Every row of the file, in its order.
    pub rows: Vec<RowReport>,
    /// The rows left out, as a CSV file with the same columns, separator and byte order mark as
    /// this one and their reason in an `error` column, ready to correct and import again; none
    /// when no row is left out.
    pub left_out: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ImportRow)]
pub struct RowReport {
    /// The line of the file the row starts on; the header is line 1.
    pub line: u64,
    pub outcome: Outcome,
    /// What the row is about: a customer's name, a site's customer and name, a requester's
    /// customer, name and first address, or an asset's customer, name and serial number.
    pub label: String,
    /// The columns whose values a row that updates changes.
    pub changes: Vec<String>,
    /// Why the row is left out.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportOutcome)]
pub enum Outcome {
    Create,
    Update,
    Unchanged,
    LeftOut,
}

/// The columns of a customers file.
pub(crate) const CUSTOMER_COLUMNS: &[&str] = &[
    "name",
    "external_system",
    "external_name",
    "legal_name",
    "tax_id",
    "tax_country",
    "address",
    "phone",
    "website",
    "notes",
];

/// The columns of a sites file: the site's customer, by name or by one of its external
/// references, and the site.
pub(crate) const SITE_COLUMNS: &[&str] = &[
    "customer",
    "customer_external_system",
    "customer_external_name",
    "name",
    "address",
];

/// The columns of a requesters file: the requester's customer, by name or by one of its external
/// references, and the requester.
pub(crate) const REQUESTER_COLUMNS: &[&str] = &[
    "customer",
    "customer_external_system",
    "customer_external_name",
    "email_addresses",
    "name",
    "phone_numbers",
    "job_title",
    "site",
    "notes",
];

/// The columns of an assets file: the asset's customer, by name or by one of its external
/// references, and the asset.
pub(crate) const ASSET_COLUMNS: &[&str] = &[
    "customer",
    "customer_external_system",
    "customer_external_name",
    "name",
    "serial_number",
    "kind",
    "site",
    "holder",
];

/// A file read: its columns and rows, and how it was written.
struct Table {
    /// Where each known column is, by its name.
    columns: HashMap<&'static str, usize>,
    rows: Vec<Row>,
    /// The header as written.
    header: Vec<String>,
    /// Where the error column is, when the file has one.
    error_at: Option<usize>,
    separator: u8,
    /// Whether the file started with a byte order mark.
    bom: bool,
}

/// One row of a file, with the line it starts on.
struct Row {
    /// The line of the file the row starts on.
    line: u64,
    cells: Vec<String>,
}

impl Table {
    /// The row's cell in `column`, trimmed; `None` when the column is absent or the cell blank.
    fn cell<'a>(&self, row: &'a Row, column: &str) -> Option<&'a str> {
        let at = *self.columns.get(column)?;
        row.cells
            .get(at)
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
    }

    /// The rows left out as a file like this one, each with its reason in the error column.
    fn left_out(&self, rows: &[(&Row, &str)]) -> Result<Option<String>> {
        if rows.is_empty() {
            return Ok(None);
        }
        let mut header = self.header.clone();
        let error_at = self.error_at.unwrap_or_else(|| {
            header.push(ERROR_COLUMN.to_owned());
            header.len() - 1
        });
        let mut out = if self.bom { BOM.to_vec() } else { Vec::new() };
        {
            let mut writer = csv::WriterBuilder::new()
                .delimiter(self.separator)
                .terminator(csv::Terminator::CRLF)
                .from_writer(&mut out);
            writer.write_record(&header)?;
            for (row, why) in rows {
                let mut cells = row.cells.clone();
                cells.resize(header.len(), String::new());
                // A spreadsheet would take a reason starting with a name like =1+1 for a formula.
                cells[error_at] = if why.starts_with(['=', '+', '-', '@']) {
                    format!("'{why}")
                } else {
                    (*why).to_owned()
                };
                writer.write_record(&cells)?;
            }
            writer.flush()?;
        }
        Ok(Some(String::from_utf8(out)?))
    }

    /// What the row is about, as the report shows it.
    fn label(&self, kind: Kind, row: &Row) -> String {
        // A customer by its name, else by its external reference.
        let customer = |name, system, reference| match (
            self.cell(row, name),
            self.cell(row, system),
            self.cell(row, reference),
        ) {
            (Some(name), _, _) => name.to_owned(),
            (None, Some(system), Some(reference)) => format!("{reference} in {system}"),
            _ => String::new(),
        };
        let of_customer = || {
            customer(
                "customer",
                "customer_external_system",
                "customer_external_name",
            )
        };
        match kind {
            Kind::Customers => customer("name", "external_system", "external_name"),
            Kind::Sites => format!(
                "{} / {}",
                of_customer(),
                self.cell(row, "name").unwrap_or_default()
            ),
            // A requester by name and first address.
            Kind::Requesters => {
                let name = self.cell(row, "name");
                let address = addresses_in(self.cell(row, "email_addresses"))
                    .into_iter()
                    .next();
                let who = match (name, address) {
                    (Some(name), Some(address)) => format!("{name} ({address})"),
                    (name, address) => name.or(address).unwrap_or_default().to_owned(),
                };
                format!("{} / {who}", of_customer())
            }
            // An asset by name and serial number.
            Kind::Assets => {
                let what = match (self.cell(row, "name"), self.cell(row, "serial_number")) {
                    (Some(name), Some(serial)) => format!("{name} ({serial})"),
                    (name, serial) => name.or(serial).unwrap_or_default().to_owned(),
                };
                format!("{} / {what}", of_customer())
            }
        }
    }
}

/// The addresses of a cell: separated by commas, semicolons, spaces or line ends, which an e-mail
/// address does not contain.
fn addresses_in(cell: Option<&str>) -> Vec<&str> {
    cell.unwrap_or_default()
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|a| !a.is_empty())
        .collect()
}

/// The phone numbers of a cell: separated by semicolons or line ends, since a number can have
/// spaces.
fn phone_numbers_in(cell: Option<&str>) -> Vec<&str> {
    cell.unwrap_or_default()
        .split([';', '\n', '\r'])
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect()
}

/// The most rows a file of an import can have: an import runs in one transaction and one request,
/// and a preview of this many rows can take tens of seconds, close to what proxies wait for.
pub const MAX_ROWS: usize = 5_000;

/// The byte order mark Excel puts at the start of a "CSV UTF-8" file.
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// The separator of a file, a semicolon or a comma: whichever its header line has more of,
/// outside quotes. The column names contain neither.
fn separator(content: &[u8]) -> u8 {
    let (mut commas, mut semicolons, mut quoted) = (0, 0, false);
    for byte in content {
        match byte {
            b'"' => quoted = !quoted,
            b'\n' | b'\r' if !quoted => break,
            b',' if !quoted => commas += 1,
            b';' if !quoted => semicolons += 1,
            _ => {}
        }
    }
    if semicolons > commas { b';' } else { b',' }
}

fn read(kind: Kind, content: &[u8]) -> Result<Table> {
    let (known, required): (&[&str], &[&str]) = match kind {
        Kind::Customers => (CUSTOMER_COLUMNS, &["name"]),
        Kind::Sites => (SITE_COLUMNS, &["name", "address"]),
        Kind::Requesters => (REQUESTER_COLUMNS, &["email_addresses"]),
        Kind::Assets => (ASSET_COLUMNS, &["name", "kind"]),
    };
    let bom = content.starts_with(BOM);
    let content = content.strip_prefix(BOM).unwrap_or(content);
    if std::str::from_utf8(content).is_err() {
        return rule(format!(
            "the {} file is not in UTF-8: save it as \"CSV UTF-8\"",
            kind.as_str()
        ));
    }
    let unreadable = |e: csv::Error| {
        Error::Rule(match e.kind() {
            csv::ErrorKind::UnequalLengths {
                pos,
                expected_len,
                len,
                ..
            } => format!(
                "the {} file: line {} has {len} cells where the header has {expected_len}",
                kind.as_str(),
                line_at(content, pos.as_ref().map_or(0, |p| p.byte() as usize)),
            ),
            _ => format!("the {} file cannot be read: {e}", kind.as_str()),
        })
    };
    let separator = separator(content);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(separator)
        .from_reader(content);
    let header: Vec<String> = reader
        .headers()
        .map_err(unreadable)?
        .iter()
        .map(str::to_owned)
        .collect();
    let mut columns = HashMap::new();
    let mut error_at = None;
    // Columns without a name, which must be empty: spreadsheets write them after the last one.
    let mut blank = Vec::new();
    let mut problems = Vec::new();
    for (at, written) in header.iter().enumerate() {
        let written = written.trim();
        let name = written.to_lowercase();
        if name.is_empty() {
            blank.push(at);
            continue;
        }
        if name == ERROR_COLUMN {
            error_at = Some(at);
            continue;
        }
        match known.iter().find(|c| **c == name) {
            Some(column) => {
                if columns.insert(*column, at).is_some() {
                    problems.push(format!("the column \"{written}\" is there twice"));
                }
            }
            None => problems.push(format!("unknown column \"{written}\"")),
        }
    }
    for column in required {
        if !columns.contains_key(column) {
            problems.push(format!("missing column \"{column}\""));
        }
    }
    // An external reference takes two columns.
    for (system, name) in [
        ("external_system", "external_name"),
        ("customer_external_system", "customer_external_name"),
    ] {
        match (columns.contains_key(system), columns.contains_key(name)) {
            (true, false) => problems.push(format!("missing column \"{name}\"")),
            (false, true) => problems.push(format!("missing column \"{system}\"")),
            _ => {}
        }
    }
    // A site's, a requester's or an asset's customer, by name or by external reference.
    if matches!(kind, Kind::Sites | Kind::Requesters | Kind::Assets)
        && !columns.contains_key("customer")
        && !columns.contains_key("customer_external_system")
        && !columns.contains_key("customer_external_name")
    {
        problems.push(
            "missing column \"customer\" (or \"customer_external_system\" and \
             \"customer_external_name\")"
                .to_owned(),
        );
    }
    if !problems.is_empty() {
        return rule(format!(
            "the {} file: {} (its columns are {})",
            kind.as_str(),
            problems.join("; "),
            known.join(", ")
        ));
    }
    let mut rows = Vec::new();
    // Lines are counted here: the reader's own count is off by one after a CRLF line end.
    let (mut counted, mut line) = (0, 1);
    for record in reader.records() {
        let record = record.map_err(unreadable)?;
        let start = row_start(content, record.position().map_or(0, |p| p.byte() as usize));
        line += newlines(&content[counted..start]);
        counted = start;
        // Spreadsheets often end with rows of empty cells.
        if record.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        if let Some(at) = blank
            .iter()
            .find(|at| record.get(**at).is_some_and(|cell| !cell.trim().is_empty()))
        {
            return rule(format!(
                "the {} file: column {} has no name, but line {line} has a value in it",
                kind.as_str(),
                at + 1
            ));
        }
        if rows.len() == MAX_ROWS {
            return rule(format!(
                "the {} file has more than {MAX_ROWS} rows: split it into several imports",
                kind.as_str()
            ));
        }
        rows.push(Row {
            line,
            cells: record.iter().map(str::to_owned).collect(),
        });
    }
    Ok(Table {
        columns,
        rows,
        header,
        error_at,
        separator,
        bom,
    })
}

/// Where the row the reader places at `byte` starts: past the line end before it.
fn row_start(content: &[u8], mut byte: usize) -> usize {
    while matches!(content.get(byte), Some(b'\r' | b'\n')) {
        byte += 1;
    }
    byte
}

fn newlines(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|b| **b == b'\n').count() as u64
}

/// The line the row the reader places at `byte` starts on.
fn line_at(content: &[u8], byte: usize) -> u64 {
    1 + newlines(&content[..row_start(content, byte).min(content.len())])
}

/// Previews or applies an import of CSV files.
pub async fn run(db: &PgPool, grant: &Grant, files: Vec<File>, mode: Mode) -> Result<Report> {
    import(db, grant, files, mode)
        .await
        .map_err(changed_meanwhile)
}

async fn import(db: &PgPool, grant: &Grant, mut files: Vec<File>, mode: Mode) -> Result<Report> {
    // Importing needs managing customers, whatever the files, and managing the catalog for
    // assets.
    grant.require(Permission::ManageCustomers)?;
    if files.iter().any(|f| f.kind == Kind::Assets) {
        grant.require(Permission::ManageCatalog)?;
    }
    if files.is_empty() {
        return rule("an import needs at least one file");
    }
    files.sort_by_key(|f| f.kind);

    if let Some(twice) = files.windows(2).find(|w| w[0].kind == w[1].kind) {
        return rule(format!(
            "an import takes one {} file",
            twice[0].kind.as_str()
        ));
    }
    let mut tables = Vec::new();
    for file in &files {
        tables.push((file.kind, read(file.kind, &file.content)?));
    }
    let mut tx = db.begin().await?;
    let mut so_far = SoFar::default();
    let mut report = Report {
        applied: mode == Mode::Apply,
        files: Vec::new(),
    };
    for (kind, table) in &tables {
        let mut rows = Vec::new();
        for row in &table.rows {
            // Each row on its own: one left out leaves nothing behind.
            let mut savepoint = tx.begin().await?;
            let mut candidates = Vec::new();
            let mut site_customer = None;
            let mut requesters = Vec::new();
            let done = match kind {
                // The database's text cannot hold it.
                _ if row.cells.iter().any(|cell| cell.contains('\0')) => {
                    rule("a cell holds a NUL character, which cannot be stored: remove it")
                }
                Kind::Customers => {
                    customer_row(&mut savepoint, table, row, &mut so_far, &mut candidates).await
                }
                Kind::Sites => {
                    site_row(&mut savepoint, table, row, &mut so_far, &mut site_customer).await
                }
                Kind::Requesters => {
                    requester_row(&mut savepoint, table, row, &mut so_far, &mut requesters).await
                }
                Kind::Assets => asset_row(&mut savepoint, table, row, &mut so_far).await,
            };
            let (outcome, changes, reason) = match left_out(done)? {
                Ok(done) => {
                    savepoint.commit().await?;
                    if *kind == Kind::Customers {
                        so_far
                            .customers_imported
                            .push(Keys::of_customer(table, row));
                    }
                    (done.outcome, done.changes, None)
                }
                Err(why) => {
                    savepoint.rollback().await?;
                    if *kind == Kind::Customers {
                        so_far.customers_left_out.push(CustomerLeftOut {
                            line: row.line,
                            customers: candidates,
                            keys: Keys::of_customer(table, row),
                        });
                    }
                    if *kind == Kind::Sites {
                        so_far.sites_left_out.push(SiteLeftOut {
                            line: row.line,
                            customer: site_customer,
                            keys: Keys::of_its_customer(table, row),
                            name: table.cell(row, "name").map(str::to_lowercase),
                        });
                    }
                    if *kind == Kind::Requesters {
                        so_far.requesters_left_out.push(RequesterLeftOut {
                            line: row.line,
                            requesters,
                            addresses: addresses_in(table.cell(row, "email_addresses"))
                                .into_iter()
                                .map(str::to_lowercase)
                                .collect(),
                        });
                    }
                    (Outcome::LeftOut, Vec::new(), Some(why))
                }
            };
            rows.push(RowReport {
                line: row.line,
                outcome,
                label: table.label(*kind, row),
                changes: changes.into_iter().map(str::to_owned).collect(),
                reason,
            });
        }
        let left_out: Vec<(&Row, &str)> = table
            .rows
            .iter()
            .zip(&rows)
            .filter_map(|(row, done)| done.reason.as_deref().map(|why| (row, why)))
            .collect();
        let left_out = table.left_out(&left_out)?;
        report.files.push(FileReport {
            kind: *kind,
            rows,
            left_out,
        });
    }
    match mode {
        Mode::Preview => tx.rollback().await?,
        Mode::Apply => tx.commit().await?,
    }
    Ok(report)
}

/// What a row did, or why it is left out: the rule it breaks. Anything else, the database
/// refusing what the core let through included, fails the whole import.
fn left_out(done: Result<Done>) -> Result<std::result::Result<Done, String>> {
    match done {
        Ok(done) => Ok(Ok(done)),
        Err(Error::Rule(why)) => Ok(Err(why)),
        Err(error) => Err(error),
    }
}

/// The help desk's data changed under the import, between a check and a write.
#[derive(Debug)]
struct Overtaken;

impl std::fmt::Display for Overtaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the help desk's data changed during the import")
    }
}

impl std::error::Error for Overtaken {}

/// A requester recognised a moment before was merged into another meanwhile, and is gone.
fn merged_meanwhile<T>() -> Result<T> {
    Err(Error::Internal(Overtaken.into()))
}

/// An import's error, or a refusal to be run again on the data as it is now when someone else
/// changed it under the import: a requester merged away, a name taken between the import's check
/// and its write, or a change waiting for what the import holds while the import waits for it.
/// An import locks the customers of its rows in the files' order, not by id as moving a
/// requester does, so the database may stop it in a deadlock (40P01) or, under a stricter
/// isolation, a serialization failure (40001).
fn changed_meanwhile(error: Error) -> Error {
    let Error::Internal(internal) = &error else {
        return error;
    };
    let database = internal
        .downcast_ref::<sqlx::Error>()
        .and_then(|e| e.as_database_error());
    let contended = database.is_some_and(|e| {
        e.is_unique_violation() || matches!(e.code().as_deref(), Some("40P01" | "40001"))
    });
    if internal.is::<Overtaken>() || contended {
        // What the database said stays in the log, should a bug of the core hide behind it.
        tracing::warn!(
            error = format!("{internal:#}"),
            "an import was refused, to be run again"
        );
        return Error::Rule(
            "the help desk's data changed during the import: reload and try again".into(),
        );
    }
    error
}

/// What the rows before have done, for the rows after them.
#[derive(Default)]
struct SoFar {
    /// The customers imported, with the line of the row that did.
    customers: HashMap<Uuid, u64>,
    /// The sites imported, with the line of the row that did.
    sites: HashMap<Uuid, u64>,
    /// The requesters imported, with the line of the row that did.
    requesters: HashMap<Uuid, u64>,
    /// The assets imported, with the line of the row that did.
    assets: HashMap<Uuid, u64>,
    /// The customer rows left out, for the rows that name their customers.
    customers_left_out: Vec<CustomerLeftOut>,
    /// The customer rows imported.
    customers_imported: Vec<Keys>,
    /// The site rows left out, for the requester rows that name their sites.
    sites_left_out: Vec<SiteLeftOut>,
    /// The requester rows left out, for the asset rows that name their holders.
    requesters_left_out: Vec<RequesterLeftOut>,
}

/// A requester row left out.
struct RequesterLeftOut {
    line: u64,
    /// The existing requesters its addresses were found to belong to, when it got that far.
    requesters: Vec<Uuid>,
    /// Its addresses as written, lowercased.
    addresses: Vec<String>,
}

/// A site row left out.
struct SiteLeftOut {
    line: u64,
    /// The customer the row was found to be about, when it got that far.
    customer: Option<Uuid>,
    /// How the row names its customer.
    keys: Keys,
    /// The site's name, ignoring case.
    name: Option<String>,
}

/// A customer row left out.
struct CustomerLeftOut {
    line: u64,
    /// The existing customers the row was found to be about: one, or several when it could be
    /// any of them. None for a row that would have created its customer.
    customers: Vec<Uuid>,
    /// How the row names its customer, for a customer it would have created.
    keys: Keys,
}

/// How a row names a customer: its name and an external reference, ignoring case.
struct Keys {
    name: Option<String>,
    reference: Option<(String, String)>,
}

impl Keys {
    fn of(table: &Table, row: &Row, name: &str, system: &str, reference: &str) -> Keys {
        let lower = |column| table.cell(row, column).map(str::to_lowercase);
        Keys {
            name: lower(name),
            reference: lower(system).zip(lower(reference)),
        }
    }

    /// How a row of a customers file names its customer.
    fn of_customer(table: &Table, row: &Row) -> Keys {
        Keys::of(table, row, "name", "external_system", "external_name")
    }

    /// How a row of a sites, requesters or assets file names its customer.
    fn of_its_customer(table: &Table, row: &Row) -> Keys {
        Keys::of(
            table,
            row,
            "customer",
            "customer_external_system",
            "customer_external_name",
        )
    }

    /// Whether both name the same customer, by name or by external reference.
    fn meet(&self, other: &Keys) -> bool {
        fn same<T: PartialEq>(a: &Option<T>, b: &Option<T>) -> bool {
            a.is_some() && a == b
        }
        same(&self.name, &other.name) || same(&self.reference, &other.reference)
    }
}

impl SoFar {
    /// The line of a customer row left out that would have created the customer a site,
    /// requester or asset row names, unless another row imported that customer.
    fn new_customer_left_out(&self, table: &Table, row: &Row) -> Option<u64> {
        let keys = Keys::of_its_customer(table, row);
        if self.customers_imported.iter().any(|k| k.meet(&keys)) {
            return None;
        }
        self.customers_left_out
            .iter()
            .find(|left_out| left_out.customers.is_empty() && left_out.keys.meet(&keys))
            .map(|left_out| left_out.line)
    }

    /// The line of a customer row left out that was about `customer`, an existing customer,
    /// unless another row imported it, as when a second row of a customer is left out.
    fn existing_customer_left_out(&self, customer: Uuid) -> Option<u64> {
        if self.customers.contains_key(&customer) {
            return None;
        }
        self.customers_left_out
            .iter()
            .find(|left_out| left_out.customers.contains(&customer))
            .map(|left_out| left_out.line)
    }

    /// The line of a requester row left out that was about the holder an asset row names: by the
    /// holder's address, or the requester it belongs to, unless another row imported them.
    fn holder_left_out(&self, address: &str, requester: Option<Uuid>) -> Option<u64> {
        if requester.is_some_and(|id| self.requesters.contains_key(&id)) {
            return None;
        }
        self.requesters_left_out
            .iter()
            .find(|left_out| {
                left_out.addresses.iter().any(|a| a == address)
                    || requester.is_some_and(|id| left_out.requesters.contains(&id))
            })
            .map(|left_out| left_out.line)
    }

    /// The line of a site row left out that was about the site of `customer` a requester row
    /// names.
    fn site_left_out(&self, table: &Table, row: &Row, customer: Uuid, name: &str) -> Option<u64> {
        let keys = Keys::of_its_customer(table, row);
        let name = name.to_lowercase();
        self.sites_left_out
            .iter()
            .find(|left_out| {
                left_out.name.as_ref() == Some(&name)
                    && (left_out.customer == Some(customer) || left_out.keys.meet(&keys))
            })
            .map(|left_out| left_out.line)
    }
}

/// What a row did.
struct Done {
    outcome: Outcome,
    changes: Vec<&'static str>,
}

impl Done {
    fn created() -> Done {
        Done {
            outcome: Outcome::Create,
            changes: Vec::new(),
        }
    }

    /// An update, or no change at all.
    fn updated(changes: Vec<&'static str>) -> Done {
        Done {
            outcome: if changes.is_empty() {
                Outcome::Unchanged
            } else {
                Outcome::Update
            },
            changes,
        }
    }
}

/// An external reference a row gives: a system and a name in it.
struct Reference<'a> {
    system: &'a str,
    name: &'a str,
}

/// The external reference of a row, in two columns: both filled, or neither.
fn reference<'a>(
    table: &Table,
    row: &'a Row,
    system: &str,
    name: &str,
) -> Result<Option<Reference<'a>>> {
    match (table.cell(row, system), table.cell(row, name)) {
        (Some(system), Some(name)) => Ok(Some(Reference { system, name })),
        (None, None) => Ok(None),
        _ => rule(format!(
            "an external reference needs both its system ({system}) and its name ({name})"
        )),
    }
}

/// The customer an external reference names.
async fn named_by(
    tx: &mut Transaction<'_, Postgres>,
    reference: &Reference<'_>,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT customer_id FROM external_references
         WHERE lower(system) = lower($1) AND lower(name) = lower($2)",
    )
    .bind(reference.system)
    .bind(reference.name)
    .fetch_optional(&mut **tx)
    .await?)
}

/// The customers with that name, ignoring case, offboarded ones too: their ids and names.
async fn with_name(tx: &mut Transaction<'_, Postgres>, name: &str) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, name FROM customers WHERE lower(name) = lower($1) ORDER BY name, id",
    )
    .bind(name)
    .fetch_all(&mut **tx)
    .await?)
}

async fn name_of(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<String> {
    Ok(
        sqlx::query_scalar("SELECT name FROM customers WHERE id = $1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?,
    )
}

/// The customer a row is about: the one its external reference names, else the one with its
/// name. A row that could be two customers is about neither. Every customer the row could be
/// about goes into `candidates`, even when it is refused.
async fn recognise(
    tx: &mut Transaction<'_, Postgres>,
    name: Option<&str>,
    reference: Option<&Reference<'_>>,
    candidates: &mut Vec<Uuid>,
) -> Result<Option<Uuid>> {
    let named = match name {
        Some(name) => with_name(tx, name).await?,
        None => Vec::new(),
    };
    candidates.extend(named.iter().map(|(id, _)| *id));
    let by_reference = match reference {
        Some(reference) => named_by(tx, reference).await?,
        None => None,
    };
    candidates.extend(by_reference);
    if let Some(reference) = reference
        && let Some(id) = by_reference
    {
        if let Some((_, other)) = named.first()
            && !named.iter().any(|(n, _)| *n == id)
        {
            let owner = name_of(tx, id).await?;
            return rule(format!(
                "{} in {} names {owner}, not {other}",
                reference.name, reference.system
            ));
        }
        return Ok(Some(id));
    }
    match named.as_slice() {
        [] => Ok(None),
        [(id, _)] => Ok(Some(*id)),
        several => rule(format!(
            "{} customers are named {}, ignoring case: tell them apart with an external reference",
            several.len(),
            name.unwrap_or_default()
        )),
    }
}

/// Refuses an offboarded customer: its sites, requesters and assets do not change, and an import
/// does not bring it back.
async fn not_offboarded(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<()> {
    let (name, offboarded): (String, bool) =
        sqlx::query_as("SELECT name, offboarded_at IS NOT NULL FROM customers WHERE id = $1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    if offboarded {
        return rule(format!(
            "{name} is offboarded: bring it back first, an import does not"
        ));
    }
    Ok(())
}

async fn customer_details(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<CustomerDetails> {
    Ok(sqlx::query_as(
        "SELECT name, legal_name, tax_id, tax_country, address, phone, website, notes
         FROM customers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?)
}

/// The columns whose values differ.
fn customer_changes(before: &CustomerDetails, after: &CustomerDetails) -> Vec<&'static str> {
    [
        ("name", before.name == after.name),
        ("legal_name", before.legal_name == after.legal_name),
        ("tax_id", before.tax_id == after.tax_id),
        ("tax_country", before.tax_country == after.tax_country),
        ("address", before.address == after.address),
        ("phone", before.phone == after.phone),
        ("website", before.website == after.website),
        ("notes", before.notes == after.notes),
    ]
    .into_iter()
    .filter(|(_, same)| !same)
    .map(|(column, _)| column)
    .collect()
}

async fn customer_row(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    so_far: &mut SoFar,
    candidates: &mut Vec<Uuid>,
) -> Result<Done> {
    let name = table.cell(row, "name");
    let reference = reference(table, row, "external_system", "external_name")?;
    let found = recognise(tx, name, reference.as_ref(), candidates).await?;
    if let Some(line) = found.and_then(|id| so_far.customers.get(&id)) {
        return rule(format!("line {line} already imports this customer"));
    }
    if let Some(id) = found {
        not_offboarded(tx, id).await?;
    }
    let (id, before) = match found {
        Some(id) => (id, Some(customer_details(tx, id).await?)),
        None => (
            crate::customers::create_in(tx, name.unwrap_or_default(), &[]).await?,
            None,
        ),
    };
    // Filled cells over the values the customer has.
    let mut details = match &before {
        Some(before) => before.clone(),
        None => customer_details(tx, id).await?,
    };
    let cell = |column| table.cell(row, column).map(str::to_owned);
    let set = |value: &mut Option<String>, column| {
        if let Some(filled) = cell(column) {
            *value = Some(filled);
        }
    };
    if let Some(name) = name {
        details.name = name.to_owned();
    }
    set(&mut details.legal_name, "legal_name");
    set(&mut details.tax_id, "tax_id");
    set(&mut details.tax_country, "tax_country");
    set(&mut details.address, "address");
    set(&mut details.phone, "phone");
    set(&mut details.website, "website");
    if let Some(notes) = cell("notes") {
        details.notes = notes;
    }
    // Only a customer that changes is written, and locked.
    let after = crate::customers::cleaned(&details)?;
    let mut changes = match &before {
        Some(before) => customer_changes(before, &after),
        None => Vec::new(),
    };
    if before.is_none() || !changes.is_empty() {
        crate::customers::update_in(tx, id, &after).await?;
    }
    if let Some(reference) = &reference
        && named_by(tx, reference).await?.is_none()
    {
        crate::external_references::add_in(tx, id, reference.system, reference.name).await?;
        changes.push("external_name");
    }
    so_far.customers.insert(id, row.line);
    Ok(match before {
        Some(_) => Done::updated(changes),
        None => Done::created(),
    })
}

/// The customer a site, requester or asset row names, which must exist and be served: by
/// external reference, else by name. `needs` says that the row needs one.
async fn named_customer(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    needs: &str,
) -> Result<Uuid> {
    let name = table.cell(row, "customer");
    let reference = reference(
        table,
        row,
        "customer_external_system",
        "customer_external_name",
    )?;
    if name.is_none() && reference.is_none() {
        return rule(needs);
    }
    if let Some(reference) = &reference
        && named_by(tx, reference).await?.is_none()
    {
        return rule(format!(
            "no customer has {} in {} as an external reference",
            reference.name, reference.system
        ));
    }
    let Some(id) = recognise(tx, name, reference.as_ref(), &mut Vec::new()).await? else {
        return rule(format!("no customer is named {}", name.unwrap_or_default()));
    };
    let owner = name_of(tx, id).await?;
    if let (Some(name), Some(reference)) = (name, &reference)
        && owner.to_lowercase() != name.to_lowercase()
    {
        return rule(format!(
            "{} in {} names {owner}, not {name}",
            reference.name, reference.system
        ));
    }
    not_offboarded(tx, id).await?;
    Ok(id)
}

/// Refuses a site, requester or asset row whose customer row is left out.
fn waits_for_customer<T>(line: u64) -> Result<T> {
    rule(format!(
        "the row of its customer, line {line} of the customers file, is left out"
    ))
}

/// A site row; `found` gets the customer it is about, once it is known.
async fn site_row(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    so_far: &mut SoFar,
    found: &mut Option<Uuid>,
) -> Result<Done> {
    if let Some(line) = so_far.new_customer_left_out(table, row) {
        return waits_for_customer(line);
    }
    let customer = named_customer(tx, table, row, "a site needs its customer").await?;
    *found = Some(customer);
    if let Some(line) = so_far.existing_customer_left_out(customer) {
        return waits_for_customer(line);
    }
    let Some(name) = table.cell(row, "name") else {
        return rule("a site needs a name");
    };
    let address = table.cell(row, "address");
    // An active site first: retired ones have freed their names.
    let found: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, name, address FROM sites
         WHERE customer_id = $1 AND lower(name) = lower($2) AND retired_at IS NULL",
    )
    .bind(customer)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((id, before_name, before_address)) = found else {
        let retired: Option<String> = sqlx::query_scalar(
            "SELECT name FROM sites WHERE customer_id = $1 AND lower(name) = lower($2)
             ORDER BY retired_at DESC LIMIT 1",
        )
        .bind(customer)
        .bind(name)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(retired) = retired {
            return rule(format!(
                "the site {retired} is retired: an import neither brings it back nor adds \
                 another site with its name"
            ));
        }
        let id = crate::sites::create_in(tx, customer, name, address.unwrap_or_default()).await?;
        so_far.sites.insert(id, row.line);
        return Ok(Done::created());
    };
    if let Some(line) = so_far.sites.get(&id) {
        return rule(format!("line {line} already imports this site"));
    }
    let address = address.unwrap_or(&before_address);
    let mut changes = Vec::new();
    if before_name != name {
        changes.push("name");
    }
    if before_address != address {
        changes.push("address");
    }
    // Only a site that changes is written, and locked.
    if !changes.is_empty() {
        crate::sites::update_in(tx, id, name, address).await?;
    }
    so_far.sites.insert(id, row.line);
    Ok(Done::updated(changes))
}

/// The requester of `customer` a row is about, by the owners of its addresses; none for a new
/// one. A row is about one requester of its customer who has not departed: an import moves,
/// merges and brings back no one, and takes no address from anyone, not even from a requester
/// who departed from another customer.
fn recognised(
    customer: Uuid,
    owners: &[crate::requesters::Owner],
    so_far: &SoFar,
) -> Result<Option<Uuid>> {
    if let Some(line) = owners
        .iter()
        .find_map(|o| so_far.requesters.get(&o.requester_id))
    {
        return rule(format!("line {line} already imports this requester"));
    }
    if let Some(other) = owners.iter().find(|o| o.customer_id != customer) {
        let address = &other.address;
        let customer = &other.customer_name;
        return rule(if other.departed {
            format!(
                "the address {address} belongs to a requester who has departed from {customer}: \
                 to give it to the requester they are now, add them on their customer's page; \
                 an import takes no address from anyone"
            )
        } else {
            format!(
                "the address {address} already belongs to a requester of {customer}, and an \
                 address belongs to one requester"
            )
        });
    }
    let Some(first) = owners.first() else {
        return Ok(None);
    };
    if let Some(other) = owners.iter().find(|o| o.requester_id != first.requester_id) {
        return rule(format!(
            "the addresses {} and {} belong to two requesters: if they are one person, merge \
             them on the page of the one that remains first; an import does not merge",
            first.address, other.address
        ));
    }
    if first.departed {
        return rule(format!(
            "the address {} belongs to a requester who has departed from {}: bring them back \
             from their page first; an import does not",
            first.address, first.customer_name
        ));
    }
    Ok(Some(first.requester_id))
}

/// The row's filled cells over a requester's details: phone numbers are added to theirs.
fn requester_merged(
    before: &RequesterDetails,
    table: &Table,
    row: &Row,
    site: Option<Uuid>,
) -> RequesterDetails {
    let mut after = before.clone();
    let cell = |column| table.cell(row, column).map(str::to_owned);
    if let Some(name) = cell("name") {
        after.name = Some(name);
    }
    for number in phone_numbers_in(table.cell(row, "phone_numbers")) {
        if !after.phone_numbers.iter().any(|n| n == number) {
            after.phone_numbers.push(number.to_owned());
        }
    }
    if let Some(job_title) = cell("job_title") {
        after.job_title = Some(job_title);
    }
    if site.is_some() {
        after.site_id = site;
    }
    if let Some(notes) = cell("notes") {
        after.notes = notes;
    }
    after
}

/// The columns whose values differ, in the order of the columns of a requesters file.
fn requester_changes(before: &RequesterDetails, after: &RequesterDetails) -> Vec<&'static str> {
    [
        ("name", before.name == after.name),
        ("phone_numbers", before.phone_numbers == after.phone_numbers),
        ("job_title", before.job_title == after.job_title),
        ("site", before.site_id == after.site_id),
        ("notes", before.notes == after.notes),
    ]
    .into_iter()
    .filter(|(_, same)| !same)
    .map(|(column, _)| column)
    .collect()
}

/// The site a requester or asset row names, by name within the row's customer: one that exists
/// and is not retired, and whose row, if the sites file has one, is not left out.
async fn named_site(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    customer: Uuid,
    so_far: &SoFar,
) -> Result<Option<Uuid>> {
    let Some(name) = table.cell(row, "site") else {
        return Ok(None);
    };
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sites
         WHERE customer_id = $1 AND lower(name) = lower($2) AND retired_at IS NULL",
    )
    .bind(customer)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await?;
    // Decided on the site found: a row left out about it holds the row back, unless another row
    // imported it.
    let imported = found.is_some_and(|id| so_far.sites.contains_key(&id));
    if !imported && let Some(line) = so_far.site_left_out(table, row, customer, name) {
        return rule(format!(
            "the row of its site, line {line} of the sites file, is left out"
        ));
    }
    if found.is_some() {
        return Ok(found);
    }
    let retired: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sites WHERE customer_id = $1 AND lower(name) = lower($2)
         ORDER BY retired_at DESC LIMIT 1",
    )
    .bind(customer)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await?;
    match retired {
        Some(retired) => rule(format!(
            "the site {retired} is retired: a retired site gets no new requesters or assets"
        )),
        None => rule(format!(
            "{} has no site named {name}",
            name_of(tx, customer).await?
        )),
    }
}

/// A requester row as recognised, before anything is locked.
struct RequesterFound {
    customer: Uuid,
    /// The existing requester the row is about; none for a new one.
    requester: Option<Uuid>,
    /// The row's addresses, clean, in their order.
    addresses: Vec<String>,
    /// Those that belong to nobody yet, which the requester gets.
    to_add: Vec<String>,
    site: Option<Uuid>,
}

/// A requester row; `owners` gets the existing requesters its addresses belong to, once they are
/// known.
async fn requester_row(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    so_far: &mut SoFar,
    owners: &mut Vec<Uuid>,
) -> Result<Done> {
    let found = recognise_requester(tx, table, row, so_far, owners).await?;
    write_requester(tx, table, row, so_far, found).await
}

/// What a requester row is about, read without locking anything; `found_owners` gets the
/// existing requesters its addresses belong to.
async fn recognise_requester(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    so_far: &SoFar,
    found_owners: &mut Vec<Uuid>,
) -> Result<RequesterFound> {
    if let Some(line) = so_far.new_customer_left_out(table, row) {
        return waits_for_customer(line);
    }
    let customer = named_customer(tx, table, row, "a requester needs its customer").await?;
    if let Some(line) = so_far.existing_customer_left_out(customer) {
        return waits_for_customer(line);
    }
    let mut addresses: Vec<String> = Vec::new();
    for written in addresses_in(table.cell(row, "email_addresses")) {
        let address = crate::requesters::clean_address(written)?;
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    if addresses.is_empty() {
        return rule("a requester needs an e-mail address");
    }
    let owners = crate::requesters::owners(&mut **tx, &addresses).await?;
    found_owners.extend(owners.iter().map(|o| o.requester_id));
    let requester = recognised(customer, &owners, so_far)?;
    let site = named_site(tx, table, row, customer, so_far).await?;
    let to_add = addresses
        .iter()
        .filter(|a| !owners.iter().any(|o| o.address == **a))
        .cloned()
        .collect();
    Ok(RequesterFound {
        customer,
        requester,
        addresses,
        to_add,
        site,
    })
}

/// Creates or updates the requester a row was recognised as; an existing one is locked and read
/// again first, and only when the row changes them.
async fn write_requester(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    so_far: &mut SoFar,
    found: RequesterFound,
) -> Result<Done> {
    let RequesterFound {
        customer,
        requester,
        addresses,
        to_add,
        site,
    } = found;
    let Some(id) = requester else {
        crate::customers::lock_active(tx, customer).await?;
        let id = crate::requesters::create_with_new_addresses(tx, customer, &addresses).await?;
        let details = requester_merged(&RequesterDetails::default(), table, row, site);
        crate::requesters::update_in(tx, id, &details).await?;
        so_far.requesters.insert(id, row.line);
        return Ok(Done::created());
    };
    let changes = |before: &RequesterDetails| {
        let mut changes = requester_changes(before, &requester_merged(before, table, row, site));
        if !to_add.is_empty() {
            changes.insert(0, "email_addresses");
        }
        changes
    };
    let Some((before, _)) = crate::requesters::stored(&mut **tx, id).await? else {
        return merged_meanwhile();
    };
    let mut changed_columns = changes(&before);
    // Only a requester that changes is written, and locked; they are read again under the lock,
    // so that a change made meanwhile is kept and the report says what is written.
    if !changed_columns.is_empty() {
        let locked = match crate::requesters::lock_with_customer(tx, id).await {
            Err(Error::NotFound(_)) => return merged_meanwhile(),
            locked => locked?,
        };
        // Moved to another customer since they were recognised: about another customer now.
        if locked != customer {
            return rule("the requester has just moved to another customer; reload and try again");
        }
        let Some((before, departed)) = crate::requesters::stored(&mut **tx, id).await? else {
            return merged_meanwhile();
        };
        if departed {
            return rule(
                "the requester has just departed: bring them back from their page first; an \
                 import does not",
            );
        }
        changed_columns = changes(&before);
        for address in &to_add {
            crate::requesters::add_new_address_in(tx, id, address).await?;
        }
        let after = requester_merged(&before, table, row, site);
        crate::requesters::update_in(tx, id, &after).await?;
    }
    so_far.requesters.insert(id, row.line);
    Ok(Done::updated(changed_columns))
}

/// The asset of `customer` a row is about, by serial number, else by name, both ignoring case;
/// none for a new one. A row is about one asset entered by hand and not retired: an import
/// chooses between no two assets, brings back no asset and never changes one an add-on is the
/// source of. Retired assets are found too, after the active ones, so that a row matching one
/// is not taken for a new asset.
async fn recognised_asset(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    name: Option<&str>,
    serial_number: Option<&str>,
) -> Result<Option<crate::catalog::StoredAsset>> {
    let mut found = None;
    if let Some(serial_number) = serial_number {
        let assets =
            crate::catalog::assets_with_serial_number(&mut **tx, customer, serial_number).await?;
        found = one_of(
            tx,
            customer,
            assets,
            "have the serial number",
            serial_number,
        )
        .await?;
    }
    // Found by its name when its serial number, if the row has one, was not found.
    let mut by_name = false;
    if found.is_none()
        && let Some(name) = name
    {
        let assets = crate::catalog::assets_named(&mut **tx, customer, name).await?;
        found = one_of(tx, customer, assets, "are named", name).await?;
        by_name = true;
    }
    let Some(asset) = found else {
        return Ok(None);
    };
    changeable(&asset, false)?;
    if by_name {
        keeps_its_serial_number(&asset, serial_number, false)?;
    }
    Ok(Some(asset))
}

/// Refuses an asset an import does not change: one an add-on is the source of, or a retired
/// one. `meanwhile` says that it became so after the row was recognised.
fn changeable(asset: &crate::catalog::StoredAsset, meanwhile: bool) -> Result<()> {
    let (comes, is) = if meanwhile {
        ("has just come", "has just been")
    } else {
        ("comes", "is")
    };
    if let Some(addon) = &asset.source_addon {
        return rule(format!(
            "{} {comes} from the add-on {addon}: only its source changes it, an import does not",
            asset.name
        ));
    }
    if asset.retired {
        return rule(format!(
            "{} {is} retired: an import neither changes a retired asset nor brings it back",
            asset.name
        ));
    }
    Ok(())
}

/// Refuses a row whose serial number would replace the one the asset has: an import replaces no
/// serial number. `meanwhile` says that the asset got its own after the row was recognised.
fn keeps_its_serial_number(
    asset: &crate::catalog::StoredAsset,
    serial_number: Option<&str>,
    meanwhile: bool,
) -> Result<()> {
    if let (Some(theirs), Some(serial_number)) = (&asset.serial_number, serial_number)
        && theirs.to_lowercase() != serial_number.to_lowercase()
    {
        let has = if meanwhile {
            "has just been given"
        } else {
            "has"
        };
        return rule(format!(
            "{} {has} the serial number {theirs}, not {serial_number}: an import does not \
             replace a serial number; correct it on the asset's page",
            asset.name
        ));
    }
    Ok(())
}

/// The asset of `assets`, found by `what` (`have the serial number`, `are named`) `value`: the
/// active one, else the most recent retired one; none when there is none, refused when several
/// active ones could be it.
async fn one_of(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    assets: Vec<crate::catalog::StoredAsset>,
    what: &str,
    value: &str,
) -> Result<Option<crate::catalog::StoredAsset>> {
    let active = assets.iter().filter(|a| !a.retired).count();
    if active > 1 {
        return rule(format!(
            "{active} assets of {} {what} {value}, ignoring case: an import does not choose \
             between them; tell them apart on their pages first",
            name_of(tx, customer).await?
        ));
    }
    Ok(assets.into_iter().next())
}

/// The asset kind a row names, which must exist; none when the cell is empty.
async fn named_kind(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
) -> Result<Option<Uuid>> {
    let Some(name) = table.cell(row, "kind") else {
        return Ok(None);
    };
    match crate::asset_kinds::named(&mut **tx, name).await? {
        Some(kind) => Ok(Some(kind)),
        None => rule(format!("there is no asset kind called {name}")),
    }
}

/// The holder an asset row names, by one of their e-mail addresses: a requester of `customer`
/// who has not departed, and whose row, if the requesters file has one, is not left out; none
/// when the cell is empty.
async fn named_holder(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    customer: Uuid,
    so_far: &SoFar,
) -> Result<Option<Uuid>> {
    let Some(written) = table.cell(row, "holder") else {
        return Ok(None);
    };
    let address = crate::requesters::clean_address(written)?;
    let owner = crate::requesters::owners(&mut **tx, std::slice::from_ref(&address))
        .await?
        .into_iter()
        .next();
    if let Some(line) = so_far.holder_left_out(&address, owner.as_ref().map(|o| o.requester_id)) {
        return rule(format!(
            "the row of its holder, line {line} of the requesters file, is left out"
        ));
    }
    match owner {
        None => rule(format!("no requester has the address {address}")),
        Some(owner) if owner.customer_id != customer => rule(format!(
            "the holder {address} is a requester of {}: an asset's holder is a requester of its \
             customer",
            owner.customer_name
        )),
        Some(owner) if owner.departed => rule(format!(
            "the holder {address} has departed from {}: a departed requester holds no asset",
            owner.customer_name
        )),
        Some(owner) => Ok(Some(owner.requester_id)),
    }
}

/// An asset row as recognised, before anything is locked.
struct AssetFound {
    customer: Uuid,
    /// The existing asset the row is about; none for a new one.
    asset: Option<crate::catalog::StoredAsset>,
    kind: Option<Uuid>,
    site: Option<Uuid>,
    holder: Option<Uuid>,
}

impl AssetFound {
    /// The row's filled cells over the asset's details.
    fn merged(
        &self,
        before: &crate::catalog::AssetFields,
        table: &Table,
        row: &Row,
    ) -> crate::catalog::AssetFields {
        let mut after = before.clone();
        if let Some(kind) = self.kind {
            after.kind_id = kind;
        }
        if let Some(name) = table.cell(row, "name") {
            after.name = name.to_owned();
        }
        if let Some(serial_number) = table.cell(row, "serial_number") {
            after.serial_number = Some(serial_number.to_owned());
        }
        if self.site.is_some() {
            after.site_id = self.site;
        }
        if self.holder.is_some() {
            after.holder_id = self.holder;
        }
        after
    }
}

/// The columns whose values differ, in the order of the columns of an assets file.
fn asset_changes(
    before: &crate::catalog::AssetFields,
    after: &crate::catalog::AssetFields,
) -> Vec<&'static str> {
    [
        ("name", before.name == after.name),
        ("serial_number", before.serial_number == after.serial_number),
        ("kind", before.kind_id == after.kind_id),
        ("site", before.site_id == after.site_id),
        ("holder", before.holder_id == after.holder_id),
    ]
    .into_iter()
    .filter(|(_, same)| !same)
    .map(|(column, _)| column)
    .collect()
}

/// An asset row: recognised, then written.
async fn asset_row(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    so_far: &mut SoFar,
) -> Result<Done> {
    let found = recognise_asset(tx, table, row, so_far).await?;
    write_asset(tx, table, row, so_far, found).await
}

/// What an asset row is about, read without locking anything.
async fn recognise_asset(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    so_far: &SoFar,
) -> Result<AssetFound> {
    if let Some(line) = so_far.new_customer_left_out(table, row) {
        return waits_for_customer(line);
    }
    let customer = named_customer(tx, table, row, "an asset needs its customer").await?;
    if let Some(line) = so_far.existing_customer_left_out(customer) {
        return waits_for_customer(line);
    }
    let name = table.cell(row, "name");
    let serial_number = table.cell(row, "serial_number");
    if name.is_none() && serial_number.is_none() {
        return rule("an asset needs a name or a serial number");
    }
    let asset = recognised_asset(tx, customer, name, serial_number).await?;
    if let Some(line) = asset.as_ref().and_then(|a| so_far.assets.get(&a.id)) {
        return rule(format!("line {line} already imports this asset"));
    }
    let kind = named_kind(tx, table, row).await?;
    let site = named_site(tx, table, row, customer, so_far).await?;
    let holder = named_holder(tx, table, row, customer, so_far).await?;
    if asset.is_none() {
        if name.is_none() {
            return rule("a new asset needs a name");
        }
        if kind.is_none() {
            return rule("a new asset needs a kind");
        }
    }
    Ok(AssetFound {
        customer,
        asset,
        kind,
        site,
        holder,
    })
}

/// Creates or updates the asset a row was recognised as; an existing one is locked and read
/// again first, and only when the row changes it.
async fn write_asset(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Row,
    so_far: &mut SoFar,
    found: AssetFound,
) -> Result<Done> {
    let customer = found.customer;
    let Some(asset) = &found.asset else {
        crate::customers::lock_active(tx, customer).await?;
        // Assets are added under their customer's lock: one added since the row was recognised
        // is seen now, and the row is about it.
        let (name, serial_number) = (table.cell(row, "name"), table.cell(row, "serial_number"));
        match recognised_asset(tx, customer, name, serial_number).await {
            Ok(None) => {}
            Ok(Some(_)) | Err(Error::Rule(_)) => return Err(Error::Internal(Overtaken.into())),
            Err(error) => return Err(error),
        }
        // A new asset has a kind and a name: a row without them was refused.
        let fields = crate::catalog::AssetFields {
            kind_id: found.kind.unwrap_or_default(),
            name: String::new(),
            serial_number: None,
            site_id: None,
            holder_id: None,
        };
        let id = crate::catalog::create_asset_in(tx, customer, &found.merged(&fields, table, row))
            .await?;
        so_far.assets.insert(id, row.line);
        return Ok(Done::created());
    };
    let before = asset.fields();
    let mut changes = asset_changes(&before, &found.merged(&before, table, row));
    // Only an asset that changes is written, and locked; it is read again under the lock, so
    // that a change made meanwhile is kept and the report says what is written. Under its
    // customer's lock, which every change to the customer's sites, requesters and assets takes
    // first, the asset is locked before its site and holder: retiring an asset, the one change
    // that does not take the customer, locks the asset alone.
    if !changes.is_empty() {
        crate::customers::lock_active(tx, customer).await?;
        let stored = crate::catalog::lock_stored(tx, asset.id).await?;
        changeable(&stored, true)?;
        keeps_its_serial_number(&stored, table.cell(row, "serial_number"), true)?;
        let before = stored.fields();
        let after = found.merged(&before, table, row);
        changes = asset_changes(&before, &after);
        crate::catalog::update_asset_in(tx, asset.id, customer, &after).await?;
    }
    so_far.assets.insert(asset.id, row.line);
    Ok(Done::updated(changes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The UI and the deployment docs list the columns these files are read with.
    #[test]
    fn the_ui_and_the_docs_list_the_columns_of_each_file() {
        let ui = include_str!("../../web/src/Import.tsx");
        let docs = include_str!("../../docs/deploy.md");
        let quoted = |text: &str, quote: char| -> Vec<String> {
            text.split(quote)
                .skip(1)
                .step_by(2)
                .map(str::to_owned)
                .collect()
        };
        for (kind, columns, section, next) in [
            (
                "customers",
                CUSTOMER_COLUMNS,
                "Customers file.",
                "Sites file.",
            ),
            ("sites", SITE_COLUMNS, "Sites file.", "Requesters file."),
            (
                "requesters",
                REQUESTER_COLUMNS,
                "Requesters file.",
                "Assets file.",
            ),
            (
                "assets",
                ASSET_COLUMNS,
                "Assets file.",
                "From the command line",
            ),
        ] {
            let listed = ui
                .split(&format!("kind: '{kind}'"))
                .nth(1)
                .and_then(|after| after.split("columns: [").nth(1))
                .and_then(|list| list.split(']').next())
                .unwrap_or_else(|| panic!("the UI lists no columns for {kind}"));
            assert_eq!(quoted(listed, '\''), columns, "the UI's {kind} columns");

            let documented = docs
                .split(section)
                .nth(1)
                .and_then(|after| after.split(next).next())
                .unwrap_or_else(|| panic!("the docs have no {section}"));
            let mut in_table: Vec<String> = documented
                .lines()
                .filter(|line| line.starts_with("| `"))
                .flat_map(|line| quoted(line.split(" | ").next().unwrap(), '`'))
                .collect();
            let mut expected: Vec<String> = columns.iter().map(|c| (*c).to_owned()).collect();
            in_table.sort();
            expected.sort();
            assert_eq!(in_table, expected, "the docs' {kind} columns");
        }
    }

    fn customers(csv: &str) -> File {
        File {
            kind: Kind::Customers,
            content: csv.as_bytes().to_vec(),
        }
    }

    fn sites(csv: &str) -> File {
        File {
            kind: Kind::Sites,
            content: csv.as_bytes().to_vec(),
        }
    }

    fn requesters(csv: &str) -> File {
        File {
            kind: Kind::Requesters,
            content: csv.as_bytes().to_vec(),
        }
    }

    fn assets(csv: &str) -> File {
        File {
            kind: Kind::Assets,
            content: csv.as_bytes().to_vec(),
        }
    }

    /// An asset as the customer's page shows it: name, serial number, kind, own site, holder's
    /// first address, and whether it is retired.
    type ShownAsset = (
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        bool,
    );

    /// The customer's assets, as its page shows them.
    async fn assets_of(db: &PgPool, customer: Uuid) -> Vec<ShownAsset> {
        crate::catalog::assets_of(db, customer)
            .await
            .unwrap()
            .into_iter()
            .map(|a| {
                (
                    a.name,
                    a.serial_number,
                    a.kind,
                    a.site.map(|s| s.name),
                    a.holder.and_then(|h| h.address),
                    a.retired_at.is_some(),
                )
            })
            .collect()
    }

    fn shown(
        name: &str,
        serial_number: Option<&str>,
        kind: &str,
        site: Option<&str>,
        holder: Option<&str>,
    ) -> ShownAsset {
        (
            name.to_owned(),
            serial_number.map(str::to_owned),
            kind.to_owned(),
            site.map(str::to_owned),
            holder.map(str::to_owned),
            false,
        )
    }

    #[sqlx::test]
    async fn assets_are_created_and_updated_by_serial_number_else_by_name(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let office = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = crate::testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let laptop = crate::asset_kinds::create(&db, "Laptop", true)
            .await
            .unwrap();
        crate::asset_kinds::create(&db, "Desktop", false)
            .await
            .unwrap();
        crate::asset_kinds::create(&db, "Printer", false)
            .await
            .unwrap();
        let details = crate::testing::asset_details;
        for fields in [
            details(laptop, "PC-0042", Some("SN-0042"), None, None),
            details(laptop, "PC-0043", None, None, None),
            details(laptop, "PC-0044", Some("SN-0044"), Some(office), Some(ada)),
        ] {
            crate::catalog::create_asset(&db, brightfield, &fields)
                .await
                .unwrap();
        }
        let file = "customer,name,serial_number,kind,site,holder\n\
            Brightfield,PC-0042 Ada,SN-0042,,head office,ADA@brightfield.test\n\
            Brightfield,PC-0043,SN-0043,desktop,,\n\
            Brightfield,Printer 2F,,printer,Head office,\n\
            Brightfield,,SN-0044,,,\n";

        let preview = import(&db, vec![assets(file)], Mode::Preview).await;
        assert_eq!(
            outcomes(&preview),
            [[
                (2, Outcome::Update, "Brightfield / PC-0042 Ada (SN-0042)"),
                (3, Outcome::Update, "Brightfield / PC-0043 (SN-0043)"),
                (4, Outcome::Create, "Brightfield / Printer 2F"),
                (5, Outcome::Unchanged, "Brightfield / SN-0044"),
            ]]
        );
        assert_eq!(
            preview.files[0].rows[0].changes,
            ["name", "site", "holder"],
            "recognised by its serial number, renamed"
        );
        assert_eq!(
            preview.files[0].rows[1].changes,
            ["serial_number", "kind"],
            "recognised by its name, given its serial number"
        );
        assert_eq!(assets_of(&db, brightfield).await.len(), 3);

        import(&db, vec![assets(file)], Mode::Apply).await;
        assert_eq!(
            assets_of(&db, brightfield).await,
            [
                shown("PC-0043", Some("SN-0043"), "Desktop", None, None),
                shown(
                    "PC-0042 Ada",
                    Some("SN-0042"),
                    "Laptop",
                    Some("Head office"),
                    Some("ada@brightfield.test")
                ),
                shown(
                    "PC-0044",
                    Some("SN-0044"),
                    "Laptop",
                    Some("Head office"),
                    Some("ada@brightfield.test")
                ),
                shown("Printer 2F", None, "Printer", Some("Head office"), None),
            ],
            "a filled cell sets a value, an empty one leaves it"
        );
    }

    #[sqlx::test]
    async fn an_asset_needs_a_kind_a_site_and_a_holder_that_exist(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
            .await
            .unwrap();
        let old = crate::sites::create(&db, brightfield, "Old lab", "Via del Glicine 3")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        crate::testing::requester(&db, harbor, "cleo@harbor.test").await;
        let lena = crate::testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::requesters::depart(&db, lena).await.unwrap();
        let gone = crate::customers::create(&db, "Ninefold", &[])
            .await
            .unwrap();
        crate::customers::offboard(&db, gone).await.unwrap();
        crate::testing::asset_kind(&db, "Laptop").await;
        crate::testing::asset(&db, brightfield, "Laptop", "PC-0042").await;

        let file = "customer,name,serial_number,kind,site,holder\n\
            Nowhere,PC-0001,,laptop,,\n\
            Ninefold,PC-0002,,laptop,,\n\
            ,PC-0003,,laptop,,\n\
            Brightfield,,,laptop,,\n\
            Brightfield,PC-0004,,,,\n\
            Brightfield,,SN-0005,laptop,,\n\
            Brightfield,PC-0006,,Tablet,,\n\
            Brightfield,PC-0007,,laptop,Quay office,\n\
            Brightfield,PC-0008,,laptop,old lab,\n\
            Brightfield,PC-0009,,laptop,,cleo@harbor.test\n\
            Brightfield,PC-0010,,laptop,,lena@brightfield.test\n\
            Brightfield,PC-0011,,laptop,,nobody@brightfield.test\n\
            Brightfield,PC-0012,,laptop,,ada@brightfield.test bo@brightfield.test\n\
            Brightfield,pc-0042,,,,lena@brightfield.test\n\
            Brightfield,PC-0013,,laptop,,\n\
            Brightfield,pc-0013,SN-0013,,,\n";
        let report = import(&db, vec![assets(file)], Mode::Apply).await;
        assert_reasons(
            &report.files[0],
            &[
                (2, "no customer is named Nowhere"),
                (3, "Ninefold is offboarded"),
                (4, "an asset needs its customer"),
                (5, "an asset needs a name or a serial number"),
                (6, "a new asset needs a kind"),
                (7, "a new asset needs a name"),
                (8, "there is no asset kind called Tablet"),
                (9, "Brightfield has no site named Quay office"),
                (10, "the site Old lab is retired"),
                (
                    11,
                    "the holder cleo@harbor.test is a requester of Harbor Lane",
                ),
                (12, "the holder lena@brightfield.test has departed"),
                (13, "no requester has the address nobody@brightfield.test"),
                (14, "is not an e-mail address"),
                (15, "the holder lena@brightfield.test has departed"),
                (17, "line 16 already imports this asset"),
            ],
        );
        assert_eq!(
            assets_of(&db, brightfield).await,
            [
                shown("PC-0013", None, "Laptop", None, None),
                shown("PC-0042", None, "Laptop", None, None),
            ]
        );
        assert!(assets_of(&db, harbor).await.is_empty());
        assert!(assets_of(&db, gone).await.is_empty());
    }

    /// A row is about one asset of its customer, entered by hand and not retired: an import
    /// chooses between no two assets, replaces no serial number, brings back no asset and never
    /// changes one an add-on is the source of, even to leave it as it is.
    #[sqlx::test]
    async fn a_row_is_about_one_active_asset_entered_by_hand(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let laptop = crate::testing::asset_kind(&db, "Laptop").await;
        let details = crate::testing::asset_details;
        let add = |name: &'static str, serial_number: Option<&'static str>| {
            let db = db.clone();
            async move {
                crate::catalog::create_asset(
                    &db,
                    brightfield,
                    &details(laptop, name, serial_number, None, None),
                )
                .await
                .unwrap()
            }
        };
        let watched = add("NAS-01", Some("SN-0100")).await;
        sqlx::query(
            "UPDATE assets SET source_addon = 'watchtower', source_ref = 'host-1' WHERE id = $1",
        )
        .bind(watched)
        .execute(&db)
        .await
        .unwrap();
        let old = add("PC-0001", Some("SN-0001")).await;
        crate::catalog::retire_asset(&db, old).await.unwrap();
        let replaced = add("PC-0002", Some("SN-0002")).await;
        crate::catalog::retire_asset(&db, replaced).await.unwrap();
        add("PC-0002", None).await;
        add("PC-0003", Some("SN-0003")).await;
        add("PC-0004", Some("sn-0003")).await;
        add("Spare", None).await;
        add("spare", None).await;
        add("PC-0005", Some("SN-0005")).await;

        let file = "customer,name,serial_number,kind,site,holder\n\
            Brightfield,,SN-0100,,,\n\
            Brightfield,nas-01,,,,\n\
            Brightfield,PC-0001 Ada,SN-0001,,,\n\
            Brightfield,PC-0001,,,,\n\
            Brightfield,PC-0002,SN-0002B,,,\n\
            Brightfield,PC-0006,SN-0003,,,\n\
            Brightfield,SPARE,,,,\n\
            Brightfield,spare,SN-0007,,,\n\
            Brightfield,PC-0005,SN-0008,,,\n\
            Brightfield,PC-0001,SN-0009,laptop,,\n";
        let report = import(&db, vec![assets(file)], Mode::Apply).await;
        assert_reasons(
            &report.files[0],
            &[
                (2, "NAS-01 comes from the add-on watchtower"),
                (3, "NAS-01 comes from the add-on watchtower"),
                (4, "PC-0001 is retired"),
                (5, "PC-0001 is retired"),
                (
                    7,
                    "2 assets of Brightfield have the serial number SN-0003, ignoring case",
                ),
                (8, "2 assets of Brightfield are named SPARE, ignoring case"),
                (9, "2 assets of Brightfield are named spare, ignoring case"),
                (10, "PC-0005 has the serial number SN-0005, not SN-0008"),
                // Named like a retired asset only: an import does not add another in its place.
                (11, "PC-0001 is retired"),
            ],
        );
        assert_eq!(
            outcomes(&report)[0][4],
            (6, Outcome::Update, "Brightfield / PC-0002 (SN-0002B)"),
            "the active asset, not the retired one with its name"
        );
        let mut shown: Vec<(String, Option<String>, bool)> = assets_of(&db, brightfield)
            .await
            .into_iter()
            .map(|(name, serial, _, _, _, retired)| (name, serial, retired))
            .collect();
        shown.sort();
        let asset = |name: &str, serial: Option<&str>, retired| {
            (name.to_owned(), serial.map(str::to_owned), retired)
        };
        assert_eq!(
            shown,
            [
                asset("NAS-01", Some("SN-0100"), false),
                asset("PC-0001", Some("SN-0001"), true),
                asset("PC-0002", Some("SN-0002"), true),
                asset("PC-0002", Some("SN-0002B"), false),
                asset("PC-0003", Some("SN-0003"), false),
                asset("PC-0004", Some("sn-0003"), false),
                asset("PC-0005", Some("SN-0005"), false),
                asset("Spare", None, false),
                asset("spare", None, false),
            ]
        );
    }

    /// Assets are taken last: a row can name a customer, site or holder an earlier file creates,
    /// in a preview too, and is held back when the row of its customer, site or holder is left
    /// out, unless another row imported it.
    #[sqlx::test]
    async fn assets_follow_the_customers_sites_and_requesters_of_the_same_import(db: PgPool) {
        crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let amberline = crate::customers::create(&db, "Amberline", &[])
            .await
            .unwrap();
        let ivo = crate::testing::requester(&db, amberline, "ivo@amberline.test").await;
        crate::requesters::add_address(&db, ivo, "ivo.r@amberline.test")
            .await
            .unwrap();
        crate::testing::requester(&db, amberline, "una@amberline.test").await;
        crate::testing::asset_kind(&db, "Laptop").await;
        let customers_file = "name,tax_country\n\
            Harbor Lane,\n\
            Copperleaf,ITA\n\
            Brightfield,ITA\n";
        let sites_file = "customer,name,address\n\
            Harbor Lane,Head office,Molo delle Reti 2\n\
            Harbor Lane,Lab,\n";
        let requesters_file = "customer,email_addresses,name,site\n\
            Harbor Lane,cleo@harbor.test,,head office\n\
            Harbor Lane,dan@harbor.test,,lab\n\
            Amberline,ivo@amberline.test,,Nowhere\n\
            Amberline,una@amberline.test,Una,\n\
            Amberline,una@amberline.test,Una R.,\n";
        let assets_file = "customer,name,kind,site,holder\n\
            Harbor Lane,PC-0001,laptop,head office,cleo@harbor.test\n\
            Copperleaf,PC-0002,laptop,,\n\
            Harbor Lane,PC-0003,laptop,lab,\n\
            Harbor Lane,PC-0004,laptop,,dan@harbor.test\n\
            Brightfield,PC-0005,laptop,,\n\
            Amberline,PC-0006,laptop,,ivo.r@amberline.test\n\
            Amberline,PC-0007,laptop,,una@amberline.test\n";
        let files = || {
            vec![
                assets(assets_file),
                requesters(requesters_file),
                sites(sites_file),
                customers(customers_file),
            ]
        };

        let preview = import(&db, files(), Mode::Preview).await;
        assert_eq!(
            preview.files.iter().map(|f| f.kind).collect::<Vec<_>>(),
            [Kind::Customers, Kind::Sites, Kind::Requesters, Kind::Assets],
            "assets last, whatever order the files come in"
        );
        let expected = [
            (2, Outcome::Create, "Harbor Lane / PC-0001"),
            (3, Outcome::LeftOut, "Copperleaf / PC-0002"),
            (4, Outcome::LeftOut, "Harbor Lane / PC-0003"),
            (5, Outcome::LeftOut, "Harbor Lane / PC-0004"),
            (6, Outcome::LeftOut, "Brightfield / PC-0005"),
            (7, Outcome::LeftOut, "Amberline / PC-0006"),
            (8, Outcome::Create, "Amberline / PC-0007"),
        ];
        assert_eq!(outcomes(&preview)[3], expected);
        assert_reasons(
            &preview.files[3],
            &[
                (
                    3,
                    "the row of its customer, line 3 of the customers file, is left out",
                ),
                (
                    4,
                    "the row of its site, line 3 of the sites file, is left out",
                ),
                (
                    5,
                    "the row of its holder, line 3 of the requesters file, is left out",
                ),
                (
                    6,
                    "the row of its customer, line 4 of the customers file, is left out",
                ),
                (
                    7,
                    "the row of its holder, line 4 of the requesters file, is left out",
                ),
            ],
        );
        assert!(assets_of(&db, amberline).await.is_empty());

        let applied = import(&db, files(), Mode::Apply).await;
        assert_eq!(outcomes(&applied)[3], expected);
        let harbor = id_of(&db, "Harbor Lane").await;
        assert_eq!(
            assets_of(&db, harbor).await,
            [shown(
                "PC-0001",
                None,
                "Laptop",
                Some("Head office"),
                Some("cleo@harbor.test")
            )],
            "at a site and with a holder the earlier files create"
        );
        assert_eq!(
            assets_of(&db, amberline).await,
            [shown(
                "PC-0007",
                None,
                "Laptop",
                None,
                Some("una@amberline.test")
            )]
        );
    }

    /// The customer's requesters, as their pages show them.
    async fn requesters_of(db: &PgPool, customer: Uuid) -> Vec<crate::requesters::RequesterDetail> {
        let mut shown = Vec::new();
        for entry in crate::customers::detail(db, customer)
            .await
            .unwrap()
            .requester_list
        {
            shown.push(crate::requesters::detail(db, entry.id).await.unwrap());
        }
        shown
    }

    /// A requester of the customer with these addresses and details.
    async fn requester_with(
        db: &PgPool,
        customer: Uuid,
        addresses: &[&str],
        details: crate::requesters::RequesterDetails,
    ) -> Uuid {
        let id = crate::testing::requester(db, customer, addresses[0]).await;
        for address in &addresses[1..] {
            crate::requesters::add_address(db, id, address)
                .await
                .unwrap();
        }
        crate::requesters::update(db, id, &details).await.unwrap();
        id
    }

    #[sqlx::test]
    async fn requesters_are_created_and_updated_by_any_of_their_addresses(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let office = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = requester_with(
            &db,
            brightfield,
            &["ada@brightfield.test", "ada.ferrini@brightfield.test"],
            crate::requesters::RequesterDetails {
                name: Some("Ada Ferrini".into()),
                phone_numbers: vec!["000 000 0100".into()],
                job_title: Some("Accountant".into()),
                notes: "Mornings only".into(),
                site_id: None,
            },
        )
        .await;
        let file = "customer,email_addresses,name,phone_numbers,job_title,site,notes\n\
            Brightfield,Ada.Ferrini@brightfield.test; a.ferrini@brightfield.test,,\
            000 000 0101,,head office,\n\
            Brightfield,\"bo@brightfield.test\nbo.vale@brightfield.test\",Bo Vale,\
            000 000 0102;000 000 0103,Technician,,Ask for the lab key\n";

        let preview = import(&db, vec![requesters(file)], Mode::Preview).await;
        assert_eq!(
            outcomes(&preview),
            [[
                (
                    2,
                    Outcome::Update,
                    "Brightfield / Ada.Ferrini@brightfield.test"
                ),
                (
                    3,
                    Outcome::Create,
                    "Brightfield / Bo Vale (bo@brightfield.test)"
                ),
            ]]
        );
        assert_eq!(
            preview.files[0].rows[0].changes,
            ["email_addresses", "phone_numbers", "site"]
        );
        assert_eq!(requesters_of(&db, brightfield).await.len(), 1);

        import(&db, vec![requesters(file)], Mode::Apply).await;
        let shown = crate::requesters::detail(&db, ada).await.unwrap();
        assert_eq!(
            shown.addresses,
            [
                "ada@brightfield.test",
                "ada.ferrini@brightfield.test",
                "a.ferrini@brightfield.test"
            ],
            "addresses are added, never taken away"
        );
        assert_eq!(
            (
                shown.name.as_deref(),
                shown.phone_numbers.as_slice(),
                shown.job_title.as_deref(),
                shown.notes.as_str(),
            ),
            (
                Some("Ada Ferrini"),
                ["000 000 0100".to_owned(), "000 000 0101".to_owned()].as_slice(),
                Some("Accountant"),
                "Mornings only"
            ),
            "a filled cell sets a value, an empty one leaves it; phone numbers are added"
        );
        assert_eq!(shown.site.map(|s| s.id), Some(office));
        let bo = requesters_of(&db, brightfield)
            .await
            .into_iter()
            .find(|r| r.id != ada)
            .unwrap();
        assert_eq!(
            (
                bo.addresses.as_slice(),
                bo.name.as_deref(),
                bo.phone_numbers.as_slice(),
                bo.job_title.as_deref(),
                bo.notes.as_str(),
                bo.site.is_none(),
            ),
            (
                [
                    "bo@brightfield.test".to_owned(),
                    "bo.vale@brightfield.test".to_owned()
                ]
                .as_slice(),
                Some("Bo Vale"),
                ["000 000 0102".to_owned(), "000 000 0103".to_owned()].as_slice(),
                Some("Technician"),
                "Ask for the lab key",
                true
            )
        );
    }

    /// A row is about one requester of its customer who has not departed: an import moves,
    /// merges and brings back no one, and takes no address from anyone.
    #[sqlx::test]
    async fn a_row_is_about_one_present_requester_of_its_customer(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        let cleo = crate::testing::requester(&db, harbor, "cleo@harbor.test").await;
        let dan = crate::testing::requester(&db, harbor, "dan@harbor.test").await;
        crate::requesters::depart(&db, dan).await.unwrap();
        crate::testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::testing::requester(&db, brightfield, "ada.f@brightfield.test").await;
        let eva = crate::testing::requester(&db, brightfield, "eva@brightfield.test").await;
        crate::requesters::depart(&db, eva).await.unwrap();

        let file = "customer,email_addresses,name\n\
            Brightfield,cleo@harbor.test,Cleo\n\
            Brightfield,dan@harbor.test,Dan\n\
            Brightfield,ada@brightfield.test ada.f@brightfield.test,Ada\n\
            Brightfield,eva@brightfield.test,Eva\n\
            Brightfield,new@brightfield.test cleo@harbor.test,New\n";
        let report = import(&db, vec![requesters(file)], Mode::Apply).await;
        assert_reasons(
            &report.files[0],
            &[
                (
                    2,
                    "cleo@harbor.test already belongs to a requester of Harbor Lane",
                ),
                (3, "departed from Harbor Lane"),
                (4, "merge them"),
                (5, "has departed from Brightfield: bring them back"),
                (
                    6,
                    "cleo@harbor.test already belongs to a requester of Harbor Lane",
                ),
            ],
        );
        let mut shown: Vec<(Vec<String>, Option<String>, bool)> = requesters_of(&db, brightfield)
            .await
            .into_iter()
            .map(|r| (r.addresses, r.name, r.departed_at.is_some()))
            .collect();
        shown.sort();
        assert_eq!(
            shown,
            [
                (vec!["ada.f@brightfield.test".to_owned()], None, false),
                (vec!["ada@brightfield.test".to_owned()], None, false),
                (vec!["eva@brightfield.test".to_owned()], None, true),
            ],
            "nothing changes at Brightfield"
        );
        let cleo = crate::requesters::detail(&db, cleo).await.unwrap();
        assert_eq!((cleo.addresses.len(), cleo.name), (1, None));
        let dan = crate::requesters::detail(&db, dan).await.unwrap();
        assert_eq!(
            dan.addresses,
            ["dan@harbor.test"],
            "the address stays with Dan"
        );
    }

    #[sqlx::test]
    async fn a_requester_needs_a_customer_and_a_site_that_exist(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let office = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let old = crate::sites::create(&db, brightfield, "Old lab", "Via del Glicine 3")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        let gone = crate::customers::create(&db, "Ninefold", &[])
            .await
            .unwrap();
        crate::customers::offboard(&db, gone).await.unwrap();

        let file = "customer,email_addresses,site\n\
            Nowhere,a@nowhere.test,\n\
            Ninefold,b@ninefold.test,\n\
            ,c@brightfield.test,\n\
            Brightfield,,\n\
            Brightfield,not-an-address,\n\
            Brightfield,d@brightfield.test,Lab\n\
            Brightfield,e@brightfield.test,old lab\n\
            Brightfield,f@brightfield.test,head office\n\
            Brightfield,F@brightfield.test,\n";
        let report = import(&db, vec![requesters(file)], Mode::Apply).await;
        assert_reasons(
            &report.files[0],
            &[
                (2, "no customer is named Nowhere"),
                (3, "Ninefold is offboarded"),
                (4, "a requester needs its customer"),
                (5, "a requester needs an e-mail address"),
                (6, "is not an e-mail address"),
                (7, "Brightfield has no site named Lab"),
                (8, "the site Old lab is retired"),
                (10, "line 9 already imports this requester"),
            ],
        );
        let shown: Vec<(Vec<String>, Option<Uuid>)> = requesters_of(&db, brightfield)
            .await
            .into_iter()
            .map(|r| (r.addresses, r.site.map(|s| s.id)))
            .collect();
        assert_eq!(
            shown,
            [(vec!["f@brightfield.test".to_owned()], Some(office))]
        );
        assert!(requesters_of(&db, gone).await.is_empty());
    }

    #[sqlx::test]
    async fn requesters_follow_the_customers_and_sites_of_the_same_import(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let customers_file = "name,tax_country\n\
            Harbor Lane,\n\
            Copperleaf,ITA\n\
            Brightfield,ITA\n";
        let sites_file = "customer,name,address\n\
            Harbor Lane,Head office,Molo delle Reti 2\n\
            Harbor Lane,Lab,\n";
        let requesters_file = "customer,email_addresses,site\n\
            Harbor Lane,cleo@harbor.test,head office\n\
            Copperleaf,lia@copperleaf.test,\n\
            Harbor Lane,dan@harbor.test,lab\n\
            Brightfield,ada@brightfield.test,\n";
        let report = import(
            &db,
            vec![
                requesters(requesters_file),
                sites(sites_file),
                customers(customers_file),
            ],
            Mode::Apply,
        )
        .await;
        assert_eq!(
            report.files.iter().map(|f| f.kind).collect::<Vec<_>>(),
            [Kind::Customers, Kind::Sites, Kind::Requesters],
            "requesters last, whatever order the files come in"
        );
        assert_eq!(
            outcomes(&report)[2][0],
            (2, Outcome::Create, "Harbor Lane / cleo@harbor.test")
        );
        assert_reasons(
            &report.files[2],
            &[
                (
                    3,
                    "the row of its customer, line 3 of the customers file, is left out",
                ),
                (
                    4,
                    "the row of its site, line 3 of the sites file, is left out",
                ),
                (
                    5,
                    "the row of its customer, line 4 of the customers file, is left out",
                ),
            ],
        );
        let harbor = id_of(&db, "Harbor Lane").await;
        let shown = requesters_of(&db, harbor).await;
        assert_eq!(shown.len(), 1);
        assert_eq!(
            (
                shown[0].addresses.as_slice(),
                shown[0].site.as_ref().map(|s| s.name.as_str())
            ),
            (
                ["cleo@harbor.test".to_owned()].as_slice(),
                Some("Head office")
            ),
            "a requester at a site the sites file creates"
        );
        assert!(requesters_of(&db, brightfield).await.is_empty());
    }

    /// The customer's sites: name, address and whether retired.
    async fn sites_of(db: &PgPool, customer: Uuid) -> Vec<(String, String, bool)> {
        crate::sites::of(db, customer)
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.name, s.address, s.retired_at.is_some()))
            .collect()
    }

    async fn id_of(db: &PgPool, name: &str) -> Uuid {
        crate::customers::list(db)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap()
            .id
    }

    fn site(name: &str, address: &str) -> (String, String, bool) {
        (name.to_owned(), address.to_owned(), false)
    }

    #[sqlx::test]
    async fn sites_are_created_and_updated_within_their_customer(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        crate::sites::create(&db, brightfield, "Warehouse", "Strada delle Gru 4")
            .await
            .unwrap();
        let customers_file = "name\nHarbor Lane\n";
        let sites_file = "customer,name,address\n\
            brightfield,head office,\"Via dei Tigli 1A\nValfiorita\"\n\
            Brightfield,Warehouse,\n\
            Brightfield,Lab,Via del Glicine 3\n\
            Harbor Lane,Head office,Molo delle Reti 2\n";
        let files = || vec![sites(sites_file), customers(customers_file)];

        let preview = import(&db, files(), Mode::Preview).await;
        assert_eq!(
            preview.files.iter().map(|f| f.kind).collect::<Vec<_>>(),
            [Kind::Customers, Kind::Sites],
            "customers first, whatever order the files come in"
        );
        assert_eq!(
            outcomes(&preview)[1],
            [
                (2, Outcome::Update, "brightfield / head office"),
                (4, Outcome::Unchanged, "Brightfield / Warehouse"),
                (5, Outcome::Create, "Brightfield / Lab"),
                (6, Outcome::Create, "Harbor Lane / Head office"),
            ],
            "a site of a customer the customers file creates is created too"
        );
        assert_eq!(
            preview.files[1].rows[0].changes,
            ["name", "address"],
            "a filled name sets the name as written"
        );
        assert_eq!(sites_of(&db, brightfield).await.len(), 2);

        import(&db, files(), Mode::Apply).await;
        assert_eq!(
            sites_of(&db, brightfield).await,
            [
                site("head office", "Via dei Tigli 1A\nValfiorita"),
                site("Lab", "Via del Glicine 3"),
                site("Warehouse", "Strada delle Gru 4"),
            ]
        );
        let harbor = id_of(&db, "Harbor Lane").await;
        assert_eq!(
            sites_of(&db, harbor).await,
            [site("Head office", "Molo delle Reti 2")]
        );
    }

    async fn import(db: &PgPool, files: Vec<File>, mode: Mode) -> Report {
        run(db, &Grant::command_line(), files, mode).await.unwrap()
    }

    fn outcomes(report: &Report) -> Vec<Vec<(u64, Outcome, &str)>> {
        report
            .files
            .iter()
            .map(|f| {
                f.rows
                    .iter()
                    .map(|r| (r.line, r.outcome, r.label.as_str()))
                    .collect()
            })
            .collect()
    }

    /// Checks that exactly the `expected` lines are left out, each for a reason containing its
    /// text.
    fn assert_reasons(file: &FileReport, expected: &[(u64, &str)]) {
        let reasons = reasons(file);
        assert_eq!(reasons.len(), expected.len(), "{reasons:?}");
        for ((line, why), (expected_line, expected)) in reasons.iter().zip(expected) {
            assert!(
                line == expected_line && why.contains(expected),
                "line {line}: {why}"
            );
        }
    }

    /// The reasons of the rows left out, by line.
    fn reasons(file: &FileReport) -> Vec<(u64, &str)> {
        file.rows
            .iter()
            .filter_map(|r| r.reason.as_deref().map(|why| (r.line, why)))
            .collect()
    }

    #[sqlx::test]
    async fn rows_that_break_a_rule_are_left_out_and_the_others_imported(db: PgPool) {
        let file = "name,tax_id,tax_country,website\n\
            Brightfield,00000000000,IT,brightfield.test\n\
            Harbor Lane,00000000000,ITA,\n\
            Copperleaf,,,javascript:alert(1)\n\
            ,,,\n\
            Ninefold,,,\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        assert_eq!(
            outcomes(&report),
            [[
                (2, Outcome::Create, "Brightfield"),
                (3, Outcome::LeftOut, "Harbor Lane"),
                (4, Outcome::LeftOut, "Copperleaf"),
                (6, Outcome::Create, "Ninefold"),
            ]],
            "a row of blank cells is no row"
        );
        let reasons = reasons(&report.files[0]);
        assert!(reasons[0].1.contains("country code"), "{reasons:?}");
        assert!(reasons[1].1.contains("web address"), "{reasons:?}");
        assert_eq!(names(&db).await, ["Brightfield", "Ninefold"]);
    }

    fn references(shown: &crate::customers::CustomerDetail) -> Vec<(&str, &str)> {
        shown
            .external_references
            .iter()
            .map(|r| (r.system.as_str(), r.name.as_str()))
            .collect()
    }

    #[sqlx::test]
    async fn customers_are_recognised_by_external_reference_else_by_name(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let details = CustomerDetails {
            name: "Brightfield".into(),
            legal_name: Some("Brightfield S.r.l.".into()),
            phone: Some("000 000 0100".into()),
            ..CustomerDetails::default()
        };
        crate::customers::update(&db, brightfield, &details)
            .await
            .unwrap();
        crate::external_references::add(&db, brightfield, "Watchtower", "BF-01")
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        crate::customers::create(&db, "Ninefold", &[])
            .await
            .unwrap();

        let file = "name,external_system,external_name,legal_name,phone,website\n\
            Brightfield Labs,watchtower,bf-01,,000 000 0199,\n\
            Harbor Lane,,,,,harbor.test\n\
            Copperleaf,Watchtower,CL-07,,,\n\
            Ninefold,,,,,\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        let changes: Vec<(Outcome, Vec<&str>)> = report.files[0]
            .rows
            .iter()
            .map(|r| (r.outcome, r.changes.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(
            changes,
            [
                (Outcome::Update, vec!["name", "phone"]),
                (Outcome::Update, vec!["website"]),
                (Outcome::Create, vec![]),
                (Outcome::Unchanged, vec![]),
            ]
        );

        let shown = crate::customers::detail(&db, brightfield).await.unwrap();
        assert_eq!(shown.summary.name, "Brightfield Labs");
        assert_eq!(
            (shown.legal_name.as_deref(), shown.phone.as_deref()),
            (Some("Brightfield S.r.l."), Some("000 000 0199")),
            "a filled cell sets a value, an empty one leaves it"
        );
        assert_eq!(references(&shown), [("Watchtower", "BF-01")]);
        let shown = crate::customers::detail(&db, harbor).await.unwrap();
        assert_eq!(shown.website.as_deref(), Some("https://harbor.test"));
        let copperleaf = crate::customers::list(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.name == "Copperleaf")
            .unwrap();
        let shown = crate::customers::detail(&db, copperleaf.id).await.unwrap();
        assert_eq!(references(&shown), [("Watchtower", "CL-07")]);
    }

    #[sqlx::test]
    async fn a_row_that_could_be_two_customers_is_left_out(db: PgPool) {
        crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        crate::customers::create(&db, "BRIGHTFIELD", &[])
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        crate::external_references::add(&db, harbor, "Watchtower", "HL")
            .await
            .unwrap();
        crate::customers::create(&db, "Copperleaf", &[])
            .await
            .unwrap();

        let file = "name,external_system,external_name,phone\n\
            brightfield,,,000 000 0100\n\
            Copperleaf,Watchtower,HL,000 000 0101\n\
            Harbor Lane,Watchtower,HL,000 000 0102\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        let reasons = reasons(&report.files[0]);
        assert_eq!(reasons.len(), 2, "{reasons:?}");
        assert_eq!(reasons[0].0, 2);
        assert!(reasons[0].1.contains("2 customers"), "{reasons:?}");
        assert_eq!(reasons[1].0, 3);
        assert!(
            reasons[1].1.contains("Harbor Lane") && reasons[1].1.contains("Copperleaf"),
            "the reference names one customer and the name another: {reasons:?}"
        );
        assert_eq!(report.files[0].rows[2].outcome, Outcome::Update);
        let phones: Vec<Option<String>> = {
            let mut phones = Vec::new();
            for c in crate::customers::list(&db).await.unwrap() {
                phones.push(crate::customers::detail(&db, c.id).await.unwrap().phone);
            }
            phones
        };
        assert_eq!(
            phones.iter().flatten().collect::<Vec<_>>(),
            ["000 000 0102"],
            "only the row that is one customer changes it"
        );
    }

    #[sqlx::test]
    async fn a_customer_is_imported_once_per_file(db: PgPool) {
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        let file = "name,external_system,external_name,phone\n\
            Harbor Lane,Ledgerly,HRB-0007,\n\
            harbor lane,,,000 000 0100\n\
            Ninefold,,,\n\
            Ninefold,,,000 000 0101\n\
            ,Ledgerly,hrb-0007,000 000 0102\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        assert_eq!(
            report.files[0]
                .rows
                .iter()
                .map(|r| r.outcome)
                .collect::<Vec<_>>(),
            [
                Outcome::Update,
                Outcome::LeftOut,
                Outcome::Create,
                Outcome::LeftOut,
                Outcome::LeftOut
            ]
        );
        let reasons = reasons(&report.files[0]);
        assert!(reasons[0].1.contains("line 2"), "{reasons:?}");
        assert!(reasons[1].1.contains("line 4"), "{reasons:?}");
        assert!(reasons[2].1.contains("line 2"), "{reasons:?}");
        assert_eq!(
            report.files[0].rows[4].label, "hrb-0007 in Ledgerly",
            "a row without a name is shown by its external reference"
        );

        let shown = crate::customers::detail(&db, harbor).await.unwrap();
        assert_eq!(
            references(&shown),
            [("Ledgerly", "HRB-0007")],
            "a reference no customer has is given to the customer with the name"
        );
        assert_eq!(shown.phone, None);
        assert_eq!(names(&db).await, ["Harbor Lane", "Ninefold"]);
    }

    /// An import never brings an offboarded customer back, nor changes it.
    #[sqlx::test]
    async fn a_row_of_an_offboarded_customer_is_left_out(db: PgPool) {
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        crate::customers::offboard(&db, harbor).await.unwrap();

        for file in [
            "name,phone\nHarbor Lane,000 000 0100\n",
            "name\nHarbor Lane\n",
        ] {
            let report = import(&db, vec![customers(file)], Mode::Apply).await;
            let reasons = reasons(&report.files[0]);
            assert!(
                reasons.len() == 1 && reasons[0].1.contains("bring it back"),
                "{reasons:?}"
            );
        }
        let shown = crate::customers::detail(&db, harbor).await.unwrap();
        assert!(shown.summary.offboarded_at.is_some());
        assert_eq!(shown.phone, None);
        assert_eq!(
            names(&db).await,
            ["Harbor Lane"],
            "and no second one is made"
        );
    }

    #[sqlx::test]
    async fn an_external_reference_names_one_customer_in_an_import_too(db: PgPool) {
        let file = "name,external_system,external_name\n\
            Copperleaf,Watchtower,CL\n\
            Ninefold,watchtower,cl\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        assert_eq!(
            outcomes(&report),
            [[
                (2, Outcome::Create, "Copperleaf"),
                (3, Outcome::LeftOut, "Ninefold")
            ]]
        );
        assert_eq!(names(&db).await, ["Copperleaf"]);
    }

    #[sqlx::test]
    async fn the_rows_left_out_come_back_as_a_file_to_correct_and_import_again(db: PgPool) {
        let file = "\u{feff}Name;tax_id;tax_country\r\n\
            Brightfield;00000000000;IT\r\n\
            Harbor Lane;\"00 000; 000\";ITA\r\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        assert_eq!(
            report.files[0].left_out.as_deref(),
            Some(
                "\u{feff}Name;tax_id;tax_country;error\r\n\
                 Harbor Lane;\"00 000; 000\";ITA;\"\"\"ITA\"\" is not a country code: use the two \
                 letters of ISO 3166-1, such as IT\"\r\n"
            ),
            "the same columns, separator and byte order mark, with the reason last"
        );

        // Corrected, it is imported; its error column is not a column of customers.
        let corrected = report.files[0]
            .left_out
            .as_deref()
            .unwrap()
            .replace(";ITA;", ";IT;");
        let report = import(&db, vec![customers(&corrected)], Mode::Apply).await;
        assert_eq!(outcomes(&report), [[(2, Outcome::Create, "Harbor Lane")]]);
        assert_eq!(report.files[0].left_out, None, "nothing left out");

        // A file that has an error column already gets the new reason in it.
        let again = "name,error,website\nNinefold,an old reason,ftp://ninefold.test\n";
        let report = import(&db, vec![customers(again)], Mode::Apply).await;
        assert_eq!(
            report.files[0].left_out.as_deref(),
            Some(
                "name,error,website\r\nNinefold,a website starts with http:// or https://,ftp://ninefold.test\r\n"
            )
        );
    }

    /// Brightfield, with its reference BF-01 in Watchtower, then an import of a customer row left
    /// out (its country is wrong) and a site row: why the site row is left out, if it is.
    async fn site_after_a_customer_row_left_out(
        db: &PgPool,
        customer_row: &str,
        site_row: &str,
    ) -> Option<String> {
        let brightfield = crate::customers::create(db, "Brightfield", &[])
            .await
            .unwrap();
        crate::external_references::add(db, brightfield, "Watchtower", "BF-01")
            .await
            .unwrap();
        let customers_file =
            format!("name,external_system,external_name,tax_country\n{customer_row}\n");
        let sites_file = format!(
            "customer,customer_external_system,customer_external_name,name,address\n{site_row}\n"
        );
        let report = import(
            db,
            vec![customers(&customers_file), sites(&sites_file)],
            Mode::Apply,
        )
        .await;
        assert_eq!(report.files[0].rows[0].outcome, Outcome::LeftOut);
        assert!(sites_of(db, brightfield).await.is_empty());
        report.files[1].rows[0].reason.clone()
    }

    #[sqlx::test]
    async fn a_site_waits_for_a_customer_row_left_out_that_named_it_by_reference(db: PgPool) {
        let why = site_after_a_customer_row_left_out(
            &db,
            ",Watchtower,BF-01,ITA",
            "Brightfield,,,Head office,Via dei Tigli 1",
        )
        .await;
        assert!(
            why.as_deref()
                .unwrap_or_default()
                .contains("line 2 of the customers file"),
            "{why:?}"
        );
    }

    #[sqlx::test]
    async fn a_site_naming_by_reference_waits_for_a_customer_row_left_out(db: PgPool) {
        let why = site_after_a_customer_row_left_out(
            &db,
            "Brightfield,,,ITA",
            ",Watchtower,BF-01,Head office,Via dei Tigli 1",
        )
        .await;
        assert!(
            why.as_deref()
                .unwrap_or_default()
                .contains("line 2 of the customers file"),
            "{why:?}"
        );
    }

    #[sqlx::test]
    async fn a_site_naming_the_old_name_waits_for_a_renaming_row_left_out(db: PgPool) {
        let why = site_after_a_customer_row_left_out(
            &db,
            "Brightfield Labs,Watchtower,BF-01,ITA",
            "Brightfield,,,Head office,Via dei Tigli 1",
        )
        .await;
        assert!(
            why.as_deref()
                .unwrap_or_default()
                .contains("line 2 of the customers file"),
            "{why:?}"
        );
    }

    #[sqlx::test]
    async fn a_second_row_of_a_customer_does_not_hold_back_its_sites(db: PgPool) {
        // One row per external reference: the second is left out, the customer is imported.
        let customers_file = "name,external_system,external_name\n\
            Brightfield,Watchtower,BF-01\n\
            Brightfield,Watchtower,BF-02\n";
        let sites_file = "customer,name,address\nBrightfield,Head office,Via dei Tigli 1\n";
        let report = import(
            &db,
            vec![customers(customers_file), sites(sites_file)],
            Mode::Apply,
        )
        .await;
        assert_eq!(report.files[0].rows[1].outcome, Outcome::LeftOut);
        assert_eq!(
            outcomes(&report)[1],
            [(2, Outcome::Create, "Brightfield / Head office")]
        );
        let brightfield = id_of(&db, "Brightfield").await;
        assert_eq!(
            sites_of(&db, brightfield).await,
            [site("Head office", "Via dei Tigli 1")]
        );
    }

    #[sqlx::test]
    async fn a_site_whose_customer_row_is_left_out_is_left_out_too(db: PgPool) {
        // Brightfield exists, but its row is left out: its sites wait for the corrected row.
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let customers_file = "name,external_system,external_name,tax_country\n\
            Brightfield,,,IT\n\
            Harbor Lane,Watchtower,HL,ITA\n\
            Copperleaf,,,\n";
        let sites_file = "customer;customer_external_system;customer_external_name;name;address\n\
            Brightfield;;;Head office;Via dei Tigli 1\n\
            ;Watchtower;HL;Head office;Molo delle Reti 2\n\
            Copperleaf;;;Head office;Corso dei Mulini 9\n";
        let report = import(
            &db,
            vec![customers(customers_file), sites(sites_file)],
            Mode::Apply,
        )
        .await;
        let reasons = reasons(&report.files[1]);
        assert_eq!(
            reasons.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
            [2, 3]
        );
        assert!(
            reasons[0].1.contains("line 2 of the customers file"),
            "{reasons:?}"
        );
        assert!(
            reasons[1].1.contains("line 3 of the customers file"),
            "{reasons:?}"
        );
        assert!(sites_of(&db, brightfield).await.is_empty());
        let copperleaf = id_of(&db, "Copperleaf").await;
        assert_eq!(
            sites_of(&db, copperleaf).await,
            [site("Head office", "Corso dei Mulini 9")]
        );
        let left_out = report.files[1].left_out.as_deref().unwrap();
        assert!(
            left_out.starts_with(
                "customer;customer_external_system;customer_external_name;name;address;error\r\n\
                 Brightfield;;;Head office;Via dei Tigli 1;"
            ),
            "{left_out}"
        );
    }

    #[sqlx::test]
    async fn a_site_needs_a_customer_that_exists_and_is_served(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        crate::external_references::add(&db, harbor, "Watchtower", "HL")
            .await
            .unwrap();
        crate::customers::create(&db, "Copperleaf", &[])
            .await
            .unwrap();
        let gone = crate::customers::create(&db, "Ninefold", &[])
            .await
            .unwrap();
        crate::customers::offboard(&db, gone).await.unwrap();

        let file = "customer,customer_external_system,customer_external_name,name,address\n\
            Nowhere,,,Lab,Via del Glicine 3\n\
            ,Watchtower,ZZ,Lab,Via del Glicine 3\n\
            Copperleaf,Watchtower,HL,Lab,Via del Glicine 3\n\
            Ninefold,,,Lab,Via del Glicine 3\n\
            Brightfield,,,,Via del Glicine 3\n\
            ,,,Lab,Via del Glicine 3\n\
            Brightfield,,,Lab,\n\
            Brightfield,,,Lab,Via del Glicine 3\n\
            Brightfield,,,LAB,Corso dei Mulini 9\n\
            ,watchtower,hl,Head office,Molo delle Reti 2\n";
        let report = import(&db, vec![sites(file)], Mode::Apply).await;
        assert_reasons(
            &report.files[0],
            &[
                (2, "no customer is named Nowhere"),
                (3, "no customer has ZZ in Watchtower"),
                (4, "names Harbor Lane, not Copperleaf"),
                (5, "offboarded"),
                (6, "a site needs a name"),
                (7, "a site needs its customer"),
                (8, "a site needs an address"),
                (10, "line 9 already imports this site"),
            ],
        );
        assert_eq!(
            sites_of(&db, brightfield).await,
            [site("Lab", "Via del Glicine 3")]
        );
        assert_eq!(
            sites_of(&db, harbor).await,
            [site("Head office", "Molo delle Reti 2")]
        );
        assert!(sites_of(&db, gone).await.is_empty());
    }

    #[sqlx::test]
    async fn a_site_named_like_a_retired_site_is_left_out(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let old = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();

        let file = "customer,name,address\nBrightfield,head office,Corso dei Mulini 9\n";
        let report = import(&db, vec![sites(file)], Mode::Apply).await;
        let reasons = reasons(&report.files[0]);
        assert!(
            reasons.len() == 1 && reasons[0].1.contains("retired"),
            "an import neither brings a retired site back nor makes another one with its name: \
             {reasons:?}"
        );
        assert_eq!(
            sites_of(&db, brightfield).await,
            [("Head office".to_owned(), "Via dei Tigli 1".to_owned(), true)]
        );

        // Beside an active site with that name, it is that one.
        crate::sites::create(&db, brightfield, "Head office", "Corso dei Mulini 9")
            .await
            .unwrap();
        let file = "customer,name,address\nBrightfield,Head office,Corso dei Mulini 9A\n";
        let report = import(&db, vec![sites(file)], Mode::Apply).await;
        assert_eq!(report.files[0].rows[0].outcome, Outcome::Update);
    }

    #[sqlx::test]
    async fn importing_needs_managing_customers(db: PgPool) {
        let others: Vec<Permission> = Permission::ALL
            .into_iter()
            .filter(|p| *p != Permission::ManageCustomers)
            .collect();
        let without = Grant {
            administrator: false,
            permissions: others,
        };
        for mode in [Mode::Preview, Mode::Apply] {
            for file in [
                customers("name\nBrightfield\n"),
                sites("customer,name,address\n"),
                requesters("customer,email_addresses\n"),
                assets("customer,name,kind\n"),
            ] {
                assert!(
                    matches!(
                        run(&db, &without, vec![file], mode).await,
                        Err(Error::Forbidden(_))
                    ),
                    "{mode:?}"
                );
            }
        }
        let with = Grant {
            administrator: false,
            permissions: vec![Permission::ManageCustomers],
        };
        run(
            &db,
            &with,
            vec![customers("name\nBrightfield\n")],
            Mode::Apply,
        )
        .await
        .unwrap();
        assert_eq!(names(&db).await, ["Brightfield"]);
    }

    /// An assets file needs managing the catalog too; without it the whole import is refused,
    /// the files of other kinds with it.
    #[sqlx::test]
    async fn importing_assets_needs_managing_the_catalog_too(db: PgPool) {
        crate::testing::asset_kind(&db, "Laptop").await;
        let files = || {
            vec![
                customers("name\nBrightfield\n"),
                assets("customer,name,kind\nBrightfield,PC-0042,Laptop\n"),
            ]
        };
        let grant = |permissions: &[Permission]| Grant {
            administrator: false,
            permissions: permissions.to_vec(),
        };
        for mode in [Mode::Preview, Mode::Apply] {
            let customers_only = grant(&[Permission::ManageCustomers]);
            assert!(
                matches!(
                    run(&db, &customers_only, files(), mode).await,
                    Err(Error::Forbidden(_))
                ),
                "{mode:?}"
            );
        }
        assert!(names(&db).await.is_empty());

        let both = grant(&[Permission::ManageCustomers, Permission::ManageCatalog]);
        let report = run(&db, &both, files(), Mode::Apply).await.unwrap();
        assert_eq!(
            outcomes(&report)[1],
            [(2, Outcome::Create, "Brightfield / PC-0042")]
        );
    }

    /// A spreadsheet takes a cell starting with = + - or @ for a formula: a reason that starts
    /// with a name could run one. The file's own cells come back as they were.
    #[sqlx::test]
    async fn a_reason_is_never_a_formula(db: PgPool) {
        for name in [
            "=Harbor Lane",
            "+Harbor Lane",
            "-Harbor Lane",
            "@Harbor Lane",
        ] {
            let harbor = crate::customers::create(&db, name, &[]).await.unwrap();
            crate::customers::offboard(&db, harbor).await.unwrap();
            let report = import(
                &db,
                vec![customers(&format!("name\n{name}\n"))],
                Mode::Apply,
            )
            .await;
            let left_out = report.files[0].left_out.clone().unwrap();
            assert!(
                left_out.contains(&format!("\r\n{name},\"'{name} is offboarded")),
                "{left_out}"
            );
        }
    }

    #[sqlx::test]
    async fn a_cell_the_database_cannot_hold_is_left_out(db: PgPool) {
        let file = "name,notes\nBrightfield,Ask for Ada\0\nHarbor Lane,\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        let reasons = reasons(&report.files[0]);
        assert!(
            reasons.len() == 1 && reasons[0].0 == 2 && reasons[0].1.contains("NUL"),
            "{reasons:?}"
        );
        assert_eq!(names(&db).await, ["Harbor Lane"]);
    }

    /// Another session that takes a site's name while the import adds it: the database refuses
    /// the second, and the import asks to be run again rather than failing on its own.
    #[sqlx::test]
    async fn an_import_overtaken_by_another_change_asks_to_try_again(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        crate::sites::create(&db, brightfield, "Old lab", "Via 1")
            .await
            .unwrap();
        // Renamed straight in the database, without the core's lock on the customer.
        let mut other = db.begin().await.unwrap();
        sqlx::query("UPDATE sites SET name = 'Lab' WHERE customer_id = $1")
            .bind(brightfield)
            .execute(&mut *other)
            .await
            .unwrap();

        let file = sites("customer,name,address\nBrightfield,Lab,Via del Glicine 3\n");
        let importing = tokio::spawn({
            let db = db.clone();
            async move { run(&db, &Grant::command_line(), vec![file], Mode::Apply).await }
        });
        // The import waits on the other session's new site, then finds the name taken.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        other.commit().await.unwrap();
        match importing.await.unwrap() {
            Err(Error::Rule(why)) => assert!(why.contains("try again"), "{why}"),
            other => panic!("not refused: {other:?}"),
        }
    }

    /// A row that changes nothing neither writes nor locks, so that an import waits only for
    /// what it changes.
    #[sqlx::test]
    async fn rows_that_change_nothing_wait_for_no_one(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let details = CustomerDetails {
            name: "Brightfield".into(),
            website: Some("brightfield.test".into()),
            ..CustomerDetails::default()
        };
        crate::customers::update(&db, brightfield, &details)
            .await
            .unwrap();
        crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();

        // An agent is changing the customer and its site meanwhile.
        let mut agent = db.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM customers WHERE id = $1 FOR UPDATE")
            .bind(brightfield)
            .execute(&mut *agent)
            .await
            .unwrap();
        sqlx::query("SELECT 1 FROM sites WHERE customer_id = $1 FOR UPDATE")
            .bind(brightfield)
            .execute(&mut *agent)
            .await
            .unwrap();

        let files = vec![
            customers("name,website\nBrightfield,brightfield.test\n"),
            sites("customer,name,address\nBrightfield,Head office,Via dei Tigli 1\n"),
        ];
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            import(&db, files, Mode::Apply),
        )
        .await
        .expect("the import waited for a row it does not change");
        let outcomes: Vec<Outcome> = report
            .files
            .iter()
            .flat_map(|f| f.rows.iter().map(|r| r.outcome))
            .collect();
        assert_eq!(outcomes, [Outcome::Unchanged, Outcome::Unchanged]);
        agent.rollback().await.unwrap();
    }

    /// A requester row that changes nothing locks neither the requester nor their customer.
    #[sqlx::test]
    async fn requester_rows_that_change_nothing_wait_for_no_one(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let office = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = requester_with(
            &db,
            brightfield,
            &["ada@brightfield.test", "ada.f@brightfield.test"],
            crate::requesters::RequesterDetails {
                name: Some("Ada Ferrini".into()),
                phone_numbers: vec!["000 000 0100".into(), "000 000 0101".into()],
                site_id: Some(office),
                ..Default::default()
            },
        )
        .await;

        // An agent is changing the customer and the requester meanwhile.
        let mut agent = db.begin().await.unwrap();
        crate::requesters::lock_with_customer(&mut agent, ada)
            .await
            .unwrap();

        let file = "customer,email_addresses,name,phone_numbers,site\n\
            Brightfield,ADA.F@brightfield.test,Ada Ferrini,000 000 0101,head office\n";
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            import(&db, vec![requesters(file)], Mode::Apply),
        )
        .await
        .expect("the import waited for a row it does not change");
        assert_eq!(outcomes(&report)[0][0].1, Outcome::Unchanged);
        agent.rollback().await.unwrap();
    }

    /// A site an agent retires while the import puts requesters there: the import waits for the
    /// customer, then finds the site retired and leaves the requesters out.
    #[sqlx::test]
    async fn a_site_retired_during_the_import_leaves_its_requesters_out(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let office = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let bo = crate::testing::requester(&db, brightfield, "bo@brightfield.test").await;
        // Retiring locks the customer first, as every change to its sites and requesters does.
        let mut agent = db.begin().await.unwrap();
        crate::customers::lock_active(&mut agent, brightfield)
            .await
            .unwrap();
        sqlx::query("UPDATE sites SET retired_at = now() WHERE id = $1")
            .bind(office)
            .execute(&mut *agent)
            .await
            .unwrap();

        let file = requesters(
            "customer,email_addresses,site\n\
             Brightfield,ada@brightfield.test,Head office\n\
             Brightfield,bo@brightfield.test,Head office\n",
        );
        let importing = tokio::spawn({
            let db = db.clone();
            async move { run(&db, &Grant::command_line(), vec![file], Mode::Apply).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        agent.commit().await.unwrap();
        let report = importing.await.unwrap().unwrap();
        let reasons = reasons(&report.files[0]);
        assert!(
            reasons.len() == 2 && reasons.iter().all(|(_, why)| why.contains("retired")),
            "{reasons:?}"
        );
        let shown = requesters_of(&db, brightfield).await;
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].id, shown[0].site.is_none()), (bo, true));
    }

    /// A requester an agent marks departed while the import changes them: the import waits for
    /// them, then finds them departed and leaves the row out.
    #[sqlx::test]
    async fn a_requester_departing_during_the_import_is_left_out(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let ada = crate::testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let mut agent = db.begin().await.unwrap();
        crate::requesters::lock_with_customer(&mut agent, ada)
            .await
            .unwrap();
        sqlx::query("UPDATE requesters SET departed_at = now() WHERE id = $1")
            .bind(ada)
            .execute(&mut *agent)
            .await
            .unwrap();

        let file = requesters(
            "customer,email_addresses,name\n\
             Brightfield,ada@brightfield.test ada.f@brightfield.test,Ada Ferrini\n",
        );
        let importing = tokio::spawn({
            let db = db.clone();
            async move { run(&db, &Grant::command_line(), vec![file], Mode::Apply).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        agent.commit().await.unwrap();
        let report = importing.await.unwrap().unwrap();
        let reasons = reasons(&report.files[0]);
        assert!(
            reasons.len() == 1 && reasons[0].1.contains("departed"),
            "{reasons:?}"
        );
        let shown = crate::requesters::detail(&db, ada).await.unwrap();
        assert_eq!((shown.addresses.len(), shown.name), (1, None));
    }

    /// A requester merged into another while the import changes them is gone: the import asks to
    /// be run again on the data as it is now.
    #[sqlx::test]
    async fn a_requester_merged_during_the_import_asks_to_try_again(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let ada = crate::testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let other = crate::testing::requester(&db, brightfield, "ada.f@brightfield.test").await;
        // The merge, as requesters::merge does it, left open.
        let mut agent = db.begin().await.unwrap();
        crate::requesters::lock_with_customer(&mut agent, ada)
            .await
            .unwrap();
        for statement in [
            "INSERT INTO requester_merges (merged_id, kept_id) VALUES ($2, $1)",
            "UPDATE requester_addresses SET requester_id = $1 WHERE requester_id = $2",
            "DELETE FROM requesters WHERE id = $2",
        ] {
            sqlx::query(statement)
                .bind(ada)
                .bind(other)
                .execute(&mut *agent)
                .await
                .unwrap();
        }

        let file =
            requesters("customer,email_addresses,name\nBrightfield,ada.f@brightfield.test,Ada\n");
        let importing = tokio::spawn({
            let db = db.clone();
            async move { run(&db, &Grant::command_line(), vec![file], Mode::Apply).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        agent.commit().await.unwrap();
        match importing.await.unwrap() {
            Err(Error::Rule(why)) => assert!(why.contains("try again"), "{why}"),
            other => panic!("not refused: {other:?}"),
        }
    }

    /// A requesters file of one row, recognised in a transaction of its own; nothing is locked yet.
    async fn recognised_row(
        db: &PgPool,
        file: &str,
    ) -> (Transaction<'static, Postgres>, Table, SoFar, RequesterFound) {
        let table = read(Kind::Requesters, file.as_bytes()).unwrap();
        let mut tx = db.begin().await.unwrap();
        let so_far = SoFar::default();
        let found = recognise_requester(&mut tx, &table, &table.rows[0], &so_far, &mut Vec::new())
            .await
            .unwrap();
        (tx, table, so_far, found)
    }

    /// A requester an agent moves to another customer after the row was recognised: the row is
    /// about a requester of another customer now, and is left out.
    #[sqlx::test]
    async fn a_requester_moved_after_being_recognised_is_left_out(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        let ada = crate::testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let (mut tx, table, mut so_far, found) = recognised_row(
            &db,
            "customer,email_addresses,name\nBrightfield,ada@brightfield.test,Ada Ferrini\n",
        )
        .await;

        crate::requesters::move_to_customer(&db, ada, harbor)
            .await
            .unwrap();
        match write_requester(&mut tx, &table, &table.rows[0], &mut so_far, found).await {
            Err(Error::Rule(why)) => assert!(why.contains("moved to another customer"), "{why}"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("written"),
        }
        tx.commit().await.unwrap();
        assert_eq!(
            crate::requesters::detail(&db, ada).await.unwrap().name,
            None
        );
    }

    /// A requester an agent merges into another after the row was recognised is gone: the import
    /// asks to be run again on the data as it is now.
    #[sqlx::test]
    async fn a_requester_merged_after_being_recognised_asks_to_try_again(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let ada = crate::testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let other = crate::testing::requester(&db, brightfield, "ada.f@brightfield.test").await;
        let (mut tx, table, mut so_far, found) = recognised_row(
            &db,
            "customer,email_addresses,name\nBrightfield,ada@brightfield.test,Ada Ferrini\n",
        )
        .await;

        crate::requesters::merge(&db, other, ada).await.unwrap();
        let written = write_requester(&mut tx, &table, &table.rows[0], &mut so_far, found).await;
        match written.map_err(changed_meanwhile) {
            Err(Error::Rule(why)) => assert!(why.contains("try again"), "{why}"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("written"),
        }
    }

    /// An asset row that changes nothing locks neither the asset nor its customer, nor does a
    /// row left out for matching an add-on's asset.
    #[sqlx::test]
    async fn asset_rows_that_change_nothing_wait_for_no_one(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let office = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = crate::testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let laptop = crate::testing::asset_kind(&db, "Laptop").await;
        let details = crate::testing::asset_details;
        let pc = crate::catalog::create_asset(
            &db,
            brightfield,
            &details(laptop, "PC-0042", Some("SN-0042"), Some(office), Some(ada)),
        )
        .await
        .unwrap();
        let nas = crate::testing::asset(&db, brightfield, "Laptop", "NAS-01").await;
        sqlx::query(
            "UPDATE assets SET source_addon = 'watchtower', source_ref = 'n' WHERE id = $1",
        )
        .bind(nas)
        .execute(&db)
        .await
        .unwrap();

        // An agent is changing the customer and its assets meanwhile.
        let mut agent = db.begin().await.unwrap();
        crate::customers::lock_active(&mut agent, brightfield)
            .await
            .unwrap();
        sqlx::query("SELECT 1 FROM assets WHERE id = ANY($1) FOR UPDATE")
            .bind([pc, nas])
            .execute(&mut *agent)
            .await
            .unwrap();

        let file = "customer,name,serial_number,kind,site,holder\n\
            Brightfield,PC-0042,SN-0042,laptop,head office,ADA@brightfield.test\n\
            Brightfield,NAS-01,,,,\n";
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            import(&db, vec![assets(file)], Mode::Apply),
        )
        .await
        .expect("the import waited for a row it does not change");
        let outcomes: Vec<Outcome> = report.files[0].rows.iter().map(|r| r.outcome).collect();
        assert_eq!(outcomes, [Outcome::Unchanged, Outcome::LeftOut]);
        agent.rollback().await.unwrap();
    }

    /// An assets file of one row, recognised in a transaction of its own; nothing is locked yet.
    async fn recognised_asset_row(
        db: &PgPool,
        file: &str,
    ) -> (Transaction<'static, Postgres>, Table, SoFar, AssetFound) {
        let table = read(Kind::Assets, file.as_bytes()).unwrap();
        let mut tx = db.begin().await.unwrap();
        let so_far = SoFar::default();
        let found = recognise_asset(&mut tx, &table, &table.rows[0], &so_far)
            .await
            .unwrap();
        (tx, table, so_far, found)
    }

    /// An asset an agent retires, or an add-on becomes the source of, after the row was
    /// recognised: the row is left out, and the asset stays as it is.
    #[sqlx::test]
    async fn an_asset_retired_or_taken_by_an_add_on_after_being_recognised_is_left_out(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        for (change, why) in [
            (
                "UPDATE assets SET retired_at = now() WHERE id = $1",
                "retired",
            ),
            (
                "UPDATE assets SET source_addon = 'watchtower', source_ref = 'pc' WHERE id = $1",
                "from the add-on watchtower",
            ),
        ] {
            let pc = crate::testing::asset(&db, brightfield, "Laptop", "PC-0042").await;
            let (mut tx, table, mut so_far, found) = recognised_asset_row(
                &db,
                "customer,name,serial_number,kind\nBrightfield,PC-0042,SN-0042,\n",
            )
            .await;

            sqlx::query(change).bind(pc).execute(&db).await.unwrap();
            match write_asset(&mut tx, &table, &table.rows[0], &mut so_far, found).await {
                Err(Error::Rule(refused)) => assert!(refused.contains(why), "{refused}"),
                Err(other) => panic!("{other:?}"),
                Ok(_) => panic!("written"),
            }
            tx.commit().await.unwrap();
            let shown = crate::catalog::assets_of(&db, brightfield).await.unwrap();
            assert_eq!(shown[0].serial_number, None, "{why}");
            sqlx::query("DELETE FROM assets")
                .execute(&db)
                .await
                .unwrap();
        }
    }

    /// An asset an agent gives another serial number after the row was recognised: the import
    /// replaces no serial number, and leaves the row out.
    #[sqlx::test]
    async fn an_asset_given_a_serial_number_after_being_recognised_is_left_out(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let laptop = crate::testing::asset_kind(&db, "Laptop").await;
        let pc = crate::testing::asset(&db, brightfield, "Laptop", "PC-0042").await;
        let (mut tx, table, mut so_far, found) = recognised_asset_row(
            &db,
            "customer,name,serial_number,kind\nBrightfield,PC-0042,SN-0042,\n",
        )
        .await;

        crate::catalog::update_asset(
            &db,
            pc,
            &crate::testing::asset_details(laptop, "PC-0042", Some("SN-0099"), None, None),
        )
        .await
        .unwrap();
        match write_asset(&mut tx, &table, &table.rows[0], &mut so_far, found).await {
            Err(Error::Rule(why)) => assert!(why.contains("serial number SN-0099"), "{why}"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("written"),
        }
        tx.commit().await.unwrap();
        assert_eq!(
            assets_of(&db, brightfield).await,
            [shown("PC-0042", Some("SN-0099"), "Laptop", None, None)]
        );
    }

    /// An asset an agent adds after a row was recognised as a new asset with its serial number:
    /// the import asks to be run again on the data as it is now, rather than add it twice.
    #[sqlx::test]
    async fn an_asset_added_after_being_recognised_as_new_asks_to_try_again(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let laptop = crate::testing::asset_kind(&db, "Laptop").await;
        let (mut tx, table, mut so_far, found) = recognised_asset_row(
            &db,
            "customer,name,serial_number,kind\nBrightfield,PC-0042,SN-0042,Laptop\n",
        )
        .await;

        crate::catalog::create_asset(
            &db,
            brightfield,
            &crate::testing::asset_details(laptop, "Laptop 7", Some("SN-0042"), None, None),
        )
        .await
        .unwrap();
        let written = write_asset(&mut tx, &table, &table.rows[0], &mut so_far, found).await;
        match written.map_err(changed_meanwhile) {
            Err(Error::Rule(why)) => assert!(why.contains("try again"), "{why}"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("written"),
        }
    }

    /// A holder an agent marks departed while the import gives them an asset: the import waits
    /// for the customer, then finds them departed and leaves the row out.
    #[sqlx::test]
    async fn a_holder_departing_during_the_import_is_left_out(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let ada = crate::testing::requester(&db, brightfield, "ada@brightfield.test").await;
        crate::testing::asset(&db, brightfield, "Laptop", "PC-0042").await;
        let mut agent = db.begin().await.unwrap();
        crate::requesters::lock_with_customer(&mut agent, ada)
            .await
            .unwrap();
        sqlx::query("UPDATE requesters SET departed_at = now() WHERE id = $1")
            .bind(ada)
            .execute(&mut *agent)
            .await
            .unwrap();

        let file = assets(
            "customer,name,kind,holder\n\
             Brightfield,PC-0042,,ada@brightfield.test\n\
             Brightfield,PC-0043,Laptop,ada@brightfield.test\n",
        );
        let importing = tokio::spawn({
            let db = db.clone();
            async move { run(&db, &Grant::command_line(), vec![file], Mode::Apply).await }
        });
        assert!(crate::testing::some_statement_waits_for_a_lock(&db).await);
        agent.commit().await.unwrap();
        let report = importing.await.unwrap().unwrap();
        let reasons = reasons(&report.files[0]);
        assert!(
            reasons.len() == 2 && reasons.iter().all(|(_, why)| why.contains("departed")),
            "{reasons:?}"
        );
        assert_eq!(
            assets_of(&db, brightfield).await,
            [shown("PC-0042", None, "Laptop", None, None)]
        );
    }

    /// The import and an agent each wait for a customer the other holds: the database stops one
    /// of them, and the import, when it is the one, asks to be run again instead of failing.
    #[sqlx::test]
    async fn an_import_in_a_deadlock_asks_to_try_again(db: PgPool) {
        // Waits on locks: it fails rather than hang when they do not go as planned.
        tokio::time::timeout(std::time::Duration::from_secs(30), import_in_a_deadlock(db))
            .await
            .expect("the deadlock was not resolved in time");
    }

    async fn import_in_a_deadlock(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        // The agent waits longer before looking for a deadlock, so that the import finds it.
        let mut agent = db.begin().await.unwrap();
        sqlx::query("SET LOCAL deadlock_timeout = '10s'")
            .execute(&mut *agent)
            .await
            .unwrap();
        crate::customers::lock_active(&mut agent, harbor)
            .await
            .unwrap();

        let file = requesters(
            "customer,email_addresses\n\
             Brightfield,ada@brightfield.test\n\
             Harbor Lane,cleo@harbor.test\n",
        );
        let importing = tokio::spawn({
            let db = db.clone();
            async move { run(&db, &Grant::command_line(), vec![file], Mode::Apply).await }
        });
        // Once the import holds Brightfield and waits for Harbor Lane, the agent asks for
        // Brightfield.
        let mut waiting = false;
        for _ in 0..200 {
            waiting = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
                                WHERE NOT l.granted AND a.datname = current_database())",
            )
            .fetch_one(&db)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(waiting, "the import never waited for Harbor Lane");
        crate::customers::lock_active(&mut agent, brightfield)
            .await
            .unwrap();
        match importing.await.unwrap() {
            Err(Error::Rule(why)) => assert!(why.contains("try again"), "{why}"),
            other => panic!("not refused: {other:?}"),
        }
        agent.rollback().await.unwrap();
        assert!(
            requesters_of(&db, brightfield).await.is_empty(),
            "the row before the deadlock is not written either"
        );
    }

    /// Applying does everything again against the database as it is then: a preview is no promise.
    #[sqlx::test]
    async fn applying_checks_everything_again(db: PgPool) {
        let file = "name,phone\nBrightfield,000 000 0100\n";
        let preview = import(&db, vec![customers(file)], Mode::Preview).await;
        assert_eq!(outcomes(&preview), [[(2, Outcome::Create, "Brightfield")]]);

        // Meanwhile an agent adds the customer by hand.
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let applied = import(&db, vec![customers(file)], Mode::Apply).await;
        assert_eq!(outcomes(&applied), [[(2, Outcome::Update, "Brightfield")]]);
        assert_eq!(names(&db).await, ["Brightfield"]);
        assert_eq!(
            crate::customers::detail(&db, brightfield)
                .await
                .unwrap()
                .phone
                .as_deref(),
            Some("000 000 0100")
        );
    }

    async fn names(db: &PgPool) -> Vec<String> {
        let mut names: Vec<String> = crate::customers::list(db)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        names.sort();
        names
    }

    #[sqlx::test]
    async fn a_preview_shows_the_customers_it_would_create_and_changes_nothing(db: PgPool) {
        let file = "name,legal_name\nBrightfield,Brightfield Labs S.r.l.\nHarbor Lane,\n";

        let preview = import(&db, vec![customers(file)], Mode::Preview).await;
        assert!(!preview.applied);
        assert_eq!(
            outcomes(&preview),
            [[
                (2, Outcome::Create, "Brightfield"),
                (3, Outcome::Create, "Harbor Lane")
            ]]
        );
        assert!(names(&db).await.is_empty(), "a preview changes nothing");

        let applied = import(&db, vec![customers(file)], Mode::Apply).await;
        assert!(applied.applied);
        assert_eq!(outcomes(&applied), outcomes(&preview));
        assert_eq!(names(&db).await, ["Brightfield", "Harbor Lane"]);
        let brightfield = crate::customers::list(&db).await.unwrap()[0].id;
        assert_eq!(
            crate::customers::detail(&db, brightfield)
                .await
                .unwrap()
                .legal_name
                .as_deref(),
            Some("Brightfield Labs S.r.l.")
        );
    }

    /// Excel saves "CSV UTF-8" with a byte order mark and, in many languages, semicolons.
    #[sqlx::test]
    async fn semicolons_and_a_byte_order_mark_are_read(db: PgPool) {
        let file = "\u{feff}name;legal_name\r\nBrightfield;Brightfield Labs, S.r.l.\r\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        assert_eq!(outcomes(&report), [[(2, Outcome::Create, "Brightfield")]]);
        let brightfield = crate::customers::list(&db).await.unwrap()[0].id;
        let shown = crate::customers::detail(&db, brightfield).await.unwrap();
        assert_eq!(
            (shown.summary.name.as_str(), shown.legal_name.as_deref()),
            ("Brightfield", Some("Brightfield Labs, S.r.l."))
        );
    }

    /// Why `files` are refused as a whole.
    async fn refused(db: &PgPool, files: Vec<File>) -> String {
        match run(db, &Grant::command_line(), files, Mode::Apply).await {
            Err(Error::Rule(why)) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[sqlx::test]
    async fn a_header_is_reported_as_written_and_blank_columns_are_ignored(db: PgPool) {
        let why = refused(
            &db,
            vec![customers("Name,Adress\nBrightfield,Via dei Tigli 1\n")],
        )
        .await;
        assert!(
            why.contains("\"Adress\""),
            "in the file's own spelling: {why}"
        );
        let why = refused(&db, vec![customers("name,NAME\nBrightfield,Harbor Lane\n")]).await;
        assert!(why.contains("\"NAME\""), "{why}");

        // Excel writes empty columns after the last one: a blank header over blank cells.
        let why = refused(
            &db,
            vec![customers("name;;\nBrightfield;;\nHarbor Lane;x;\n")],
        )
        .await;
        assert!(
            why.contains("column 2"),
            "a blank header over a filled cell: {why}"
        );
        let report = import(
            &db,
            vec![customers(
                "name;phone;;\r\nBrightfield;000 000 0100;;\r\nHarbor Lane;;; \r\n",
            )],
            Mode::Apply,
        )
        .await;
        assert_eq!(
            outcomes(&report),
            [[
                (2, Outcome::Create, "Brightfield"),
                (3, Outcome::Create, "Harbor Lane")
            ]]
        );
    }

    #[sqlx::test]
    async fn a_sites_file_names_customers_by_name_or_by_reference(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        crate::external_references::add(&db, brightfield, "Watchtower", "BF-01")
            .await
            .unwrap();
        let file = "customer_external_system,customer_external_name,name,address\n\
            Watchtower,BF-01,Head office,Via dei Tigli 1\n";
        let report = import(&db, vec![sites(file)], Mode::Apply).await;
        assert_eq!(
            outcomes(&report),
            [[(2, Outcome::Create, "BF-01 in Watchtower / Head office")]]
        );

        let why = refused(&db, vec![sites("name,address\nLab,Via del Glicine 3\n")]).await;
        assert!(why.contains("\"customer\""), "{why}");
        let why = refused(
            &db,
            vec![sites(
                "customer_external_system,name,address\nWatchtower,Lab,Via del Glicine 3\n",
            )],
        )
        .await;
        assert!(why.contains("\"customer_external_name\""), "{why}");
    }

    #[sqlx::test]
    async fn an_unknown_or_missing_column_is_reported_before_anything_is_imported(db: PgPool) {
        let why = refused(
            &db,
            vec![customers("name,adress\nBrightfield,Via dei Tigli 1\n")],
        )
        .await;
        assert!(why.contains("\"adress\""), "{why}");
        let why = refused(&db, vec![customers("legal_name\nBrightfield Labs\n")]).await;
        assert!(why.contains("\"name\""), "{why}");
        let why = refused(&db, vec![customers("name,Name\nBrightfield,Harbor Lane\n")]).await;
        assert!(why.contains("twice"), "{why}");
        // A later file's columns stop the earlier files too.
        let why = refused(
            &db,
            vec![
                customers("name\nBrightfield\n"),
                sites("customer,name\nBrightfield,Lab\n"),
            ],
        )
        .await;
        assert!(
            why.contains("sites file") && why.contains("\"address\""),
            "{why}"
        );
        assert!(names(&db).await.is_empty());

        // Column names ignore case and the spaces around them.
        import(
            &db,
            vec![customers(" Name ,LEGAL_NAME\nBrightfield,\n")],
            Mode::Apply,
        )
        .await;
        assert_eq!(names(&db).await, ["Brightfield"]);
    }

    /// An assets file has the columns a new asset needs, its customer's, and no others; it takes
    /// as many rows as any other file.
    #[sqlx::test]
    async fn an_assets_file_is_checked_like_the_others(db: PgPool) {
        for (file, expected) in [
            ("name,kind\nPC-0042,Laptop\n", "\"customer\""),
            ("customer,kind\nBrightfield,Laptop\n", "\"name\""),
            ("customer,name\nBrightfield,PC-0042\n", "\"kind\""),
            (
                "customer,name,kind,owner\nBrightfield,PC-0042,Laptop,Ada\n",
                "unknown column \"owner\"",
            ),
            (
                "customer_external_system,name,kind\nWatchtower,PC-0042,Laptop\n",
                "\"customer_external_name\"",
            ),
        ] {
            let why = refused(&db, vec![customers("name\nBrightfield\n"), assets(file)]).await;
            assert!(
                why.contains("the assets file") && why.contains(expected),
                "{why}"
            );
        }
        let mut many = String::from("customer,name,kind\n");
        for n in 0..=MAX_ROWS {
            many.push_str(&format!("Brightfield,PC-{n},Laptop\n"));
        }
        let why = refused(&db, vec![assets(&many)]).await;
        assert!(why.contains("assets file has more than 5000 rows"), "{why}");
        assert!(names(&db).await.is_empty(), "nothing is imported");
    }

    #[sqlx::test]
    async fn a_file_that_cannot_be_read_is_refused_before_anything_is_imported(db: PgPool) {
        // Excel's plain "CSV" is often Windows-1252, not UTF-8.
        let latin = File {
            kind: Kind::Customers,
            content: b"name\nBrightfield\nCaff\xe8 Nero\n".to_vec(),
        };
        let why = refused(&db, vec![latin]).await;
        assert!(why.contains("UTF-8"), "{why}");

        let why = refused(&db, vec![customers("name,phone\nBrightfield\n")]).await;
        assert!(why.contains("line 2"), "{why}");

        let why = refused(&db, vec![customers("name,phone\n\"Brightfield,000\n")]).await;
        assert!(why.contains("line 2"), "{why}");

        let mut many = String::from("name\n");
        for n in 0..=MAX_ROWS {
            many.push_str(&format!("Customer {n}\n"));
        }
        let why = refused(&db, vec![customers(&many)]).await;
        assert!(why.contains("5000"), "{why}");

        let why = refused(
            &db,
            vec![
                customers("name\nBrightfield\n"),
                customers("name\nHarbor Lane\n"),
            ],
        )
        .await;
        assert!(why.contains("one customers file"), "{why}");
        assert!(names(&db).await.is_empty());
    }

    #[sqlx::test]
    async fn quoted_cells_keep_separators_quotes_and_line_ends(db: PgPool) {
        let file = "name,address,notes\n\
            \"Harbor Lane, Ltd\",\"Molo delle Reti 2\nPortoalto\",\"Ask for \"\"Bo\"\"; mornings only\"\n\
            Brightfield,,\n";
        let report = import(&db, vec![customers(file)], Mode::Apply).await;
        assert_eq!(
            outcomes(&report),
            [[
                (2, Outcome::Create, "Harbor Lane, Ltd"),
                (4, Outcome::Create, "Brightfield")
            ]],
            "a row starts on the line after the line ends inside the one before"
        );
        let harbor = crate::customers::list(&db).await.unwrap()[1].id;
        let shown = crate::customers::detail(&db, harbor).await.unwrap();
        assert_eq!(
            (shown.address.as_deref(), shown.notes.as_str()),
            (
                Some("Molo delle Reti 2\nPortoalto"),
                "Ask for \"Bo\"; mornings only"
            )
        );
    }
}
