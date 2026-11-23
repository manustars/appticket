//! Importing customers and their sites from CSV files. Rules are in docs/domain-rules.md
//! ("Importing").

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::{Acquire, PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::customers::CustomerDetails;
use crate::error::{Error, Result, rule};
use crate::roles::{Grant, Permission};

/// What a file of an import holds, one kind per file. Files are taken in this order, so that a
/// row can refer to something an earlier file creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportKind)]
pub enum Kind {
    Customers,
    Sites,
}

impl Kind {
    /// What importing this kind needs.
    fn permission(self) -> Permission {
        match self {
            Kind::Customers | Kind::Sites => Permission::ManageCustomers,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Customers => "customers",
            Kind::Sites => "sites",
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
    /// What the row is about: a customer's name, or a site's customer and name.
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

/// A file read: its columns and rows, and how it was written.
struct Table {
    /// Where each known column is, by its name.
    columns: HashMap<&'static str, usize>,
    rows: Vec<Line>,
    /// The header as written.
    header: Vec<String>,
    /// Where the error column is, when the file has one.
    error_at: Option<usize>,
    separator: u8,
    /// Whether the file started with a byte order mark.
    bom: bool,
}

/// One row of a file.
struct Line {
    /// The line of the file the row starts on.
    line: u64,
    cells: Vec<String>,
}

impl Table {
    /// The row's cell in `column`, trimmed; `None` when the column is absent or the cell blank.
    fn cell<'a>(&self, row: &'a Line, column: &str) -> Option<&'a str> {
        let at = *self.columns.get(column)?;
        row.cells
            .get(at)
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
    }

    /// The rows left out as a file like this one, each with its reason in the error column.
    fn left_out(&self, rows: &[(&Line, &str)]) -> Result<Option<String>> {
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
                cells[error_at] = (*why).to_owned();
                writer.write_record(&cells)?;
            }
            writer.flush()?;
        }
        Ok(Some(String::from_utf8(out)?))
    }

    /// What the row is about, as the report shows it.
    fn label(&self, kind: Kind, row: &Line) -> String {
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
        match kind {
            Kind::Customers => customer("name", "external_system", "external_name"),
            Kind::Sites => format!(
                "{} / {}",
                customer(
                    "customer",
                    "customer_external_system",
                    "customer_external_name"
                ),
                self.cell(row, "name").unwrap_or_default()
            ),
        }
    }
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
    // A site's customer, by name or by external reference.
    if kind == Kind::Sites
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
        rows.push(Line {
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
pub async fn run(db: &PgPool, grant: &Grant, mut files: Vec<File>, mode: Mode) -> Result<Report> {
    // Importing needs managing customers, whatever the files; some kinds need more.
    grant.require(Permission::ManageCustomers)?;
    for file in &files {
        grant.require(file.kind.permission())?;
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
            let done = match kind {
                // The database's text cannot hold it.
                _ if row.cells.iter().any(|cell| cell.contains('\0')) => {
                    rule("a cell holds a NUL character, which cannot be stored: remove it")
                }
                Kind::Customers => {
                    customer_row(&mut savepoint, table, row, &mut so_far, &mut candidates).await
                }
                Kind::Sites => site_row(&mut savepoint, table, row, &mut so_far).await,
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
        let left_out: Vec<(&Line, &str)> = table
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
        // Someone else took a name the import was given a moment before, between its check and
        // its write: the whole import is refused, to be run again on the data as it is now.
        Err(Error::Internal(error))
            if error
                .downcast_ref::<sqlx::Error>()
                .and_then(|e| e.as_database_error())
                .is_some_and(|e| e.is_unique_violation()) =>
        {
            rule("the help desk's data changed during the import: reload and try again")
        }
        Err(error) => Err(error),
    }
}

/// What the rows before have done, for the rows after them.
#[derive(Default)]
struct SoFar {
    /// The customers imported, with the line of the row that did.
    customers: HashMap<Uuid, u64>,
    /// The sites imported, with the line of the row that did.
    sites: HashMap<Uuid, u64>,
    /// The customer rows left out, for the rows that name their customers.
    customers_left_out: Vec<CustomerLeftOut>,
    /// The customer rows imported.
    customers_imported: Vec<Keys>,
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
    fn of(table: &Table, row: &Line, name: &str, system: &str, reference: &str) -> Keys {
        let lower = |column| table.cell(row, column).map(str::to_lowercase);
        Keys {
            name: lower(name),
            reference: lower(system).zip(lower(reference)),
        }
    }

    /// How a row of a customers file names its customer.
    fn of_customer(table: &Table, row: &Line) -> Keys {
        Keys::of(table, row, "name", "external_system", "external_name")
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
    /// The line of a customer row left out that would have created the customer a site row
    /// names, unless another row imported that customer.
    fn new_customer_left_out(&self, table: &Table, row: &Line) -> Option<u64> {
        let keys = Keys::of(
            table,
            row,
            "customer",
            "customer_external_system",
            "customer_external_name",
        );
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
    row: &'a Line,
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

/// Refuses an offboarded customer: its sites and requesters do not change, and an import does
/// not bring it back.
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
    row: &Line,
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

/// The customer a site row names, which must exist: by external reference, else by name.
async fn site_customer(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Line,
) -> Result<Uuid> {
    let name = table.cell(row, "customer");
    let reference = reference(
        table,
        row,
        "customer_external_system",
        "customer_external_name",
    )?;
    if name.is_none() && reference.is_none() {
        return rule("a site needs its customer");
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

async fn site_row(
    tx: &mut Transaction<'_, Postgres>,
    table: &Table,
    row: &Line,
    so_far: &mut SoFar,
) -> Result<Done> {
    let waits = |line| {
        rule(format!(
            "the row of its customer, line {line} of the customers file, is left out"
        ))
    };
    if let Some(line) = so_far.new_customer_left_out(table, row) {
        return waits(line);
    }
    let customer = site_customer(tx, table, row).await?;
    if let Some(line) = so_far.existing_customer_left_out(customer) {
        return waits(line);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let reasons = reasons(&report.files[0]);
        let expected = [
            (2, "no customer is named Nowhere"),
            (3, "no customer has ZZ in Watchtower"),
            (4, "names Harbor Lane, not Copperleaf"),
            (5, "offboarded"),
            (6, "a site needs a name"),
            (7, "a site needs its customer"),
            (8, "a site needs an address"),
            (10, "line 9 already imports this site"),
        ];
        assert_eq!(reasons.len(), expected.len(), "{reasons:?}");
        for ((line, why), (expected_line, expected)) in reasons.iter().zip(expected) {
            assert!(
                *line == expected_line && why.contains(expected),
                "line {line}: {why}"
            );
        }
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
