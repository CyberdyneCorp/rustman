//! Subgraph pagination (add-evm-toolkit "Pagination helpers"): the
//! operation is sent again with its cursor variable moved on, until a page
//! returns fewer than `first` items or `--max-pages` is reached, and the
//! pages are concatenated into one array per paginated field.
//!
//! A paginated field is a top-level field with a `first` argument. With
//! `id_gt` (the default) its cursor is the variable in `where: {id_gt:
//! $var}`, set to the last item's `id`; with `skip` it is the variable in
//! `skip: $var`, moved on by `first`.

use apollo_compiler::ast::{self, Definition, Selection, Value as AstValue};
use serde_json::{Map, Value};

use crate::operation::DOCUMENT_PATH;

/// Pages read at most, unless `--max-pages` says otherwise.
pub const DEFAULT_MAX_PAGES: u32 = 20;

/// The largest `--max-pages`.
pub const MAX_PAGES_LIMIT: u32 = 1000;

/// Page bodies held in memory at most across a paginated run: paging stops
/// (with [`bytes_capped_warning`]) once the pages read reach it.
pub const MAX_PAGED_BYTES: u64 = 64 * 1024 * 1024;

/// How the cursor moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageMode {
    /// `where: {id_gt: $last}`, set to the last item's `id`.
    #[default]
    IdGt,
    /// `skip: $skip`, moved on by `first`.
    Skip,
}

impl PageMode {
    /// `id_gt` or `skip`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "id_gt" => Some(Self::IdGt),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// `--paginate [id_gt|skip]` with `--max-pages`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paginate {
    pub mode: PageMode,
    pub max_pages: u32,
}

impl Default for Paginate {
    fn default() -> Self {
        Self {
            mode: PageMode::IdGt,
            max_pages: DEFAULT_MAX_PAGES,
        }
    }
}

/// The warning when pages remained at the cap.
pub fn capped_warning(max_pages: u32) -> String {
    format!("stopped at --max-pages {max_pages}")
}

/// The warning when pages remained at [`MAX_PAGED_BYTES`].
pub fn bytes_capped_warning(pages: u32) -> String {
    format!(
        "stopped after {pages} pages: they reached the {} MiB pagination limit",
        MAX_PAGED_BYTES / (1024 * 1024)
    )
}

/// The error added to the kept pages when page `failed` could not be read.
pub fn partial_error(failed: u32, why: &str) -> String {
    let kept = failed - 1;
    let pages = if kept == 1 { "page" } else { "pages" };
    format!("--paginate: page {failed} failed ({why}); keeping the {kept} {pages} read before it")
}

/// One paginated top-level field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagedField {
    /// Its response key (the alias, else the name).
    pub key: String,
    /// The page size.
    pub first: u64,
    /// The cursor variable, without `$`.
    pub cursor: String,
}

/// The paginated fields of the operation `operation_name` of `document`.
pub fn plan(
    document: &str,
    operation_name: Option<&str>,
    variables: &Map<String, Value>,
    mode: PageMode,
) -> Result<Vec<PagedField>, String> {
    let ast = match ast::Document::parse(document, DOCUMENT_PATH) {
        Ok(ast) => ast,
        Err(with_errors) => with_errors.partial,
    };
    let operation = ast
        .definitions
        .iter()
        .filter_map(|d| match d {
            Definition::OperationDefinition(op) => Some(op),
            _ => None,
        })
        .find(|op| operation_name.is_none() || op.name.as_deref() == operation_name)
        .ok_or("the document has no such operation")?;
    let mut fields = Vec::new();
    for selection in &operation.selection_set {
        if let Selection::Field(field) = selection
            && let Some(first) = argument(field, "first")
        {
            fields.push(paged_field(field, first, variables, mode)?);
        }
    }
    if fields.is_empty() {
        return Err("no top-level field takes `first`".to_owned());
    }
    Ok(fields)
}

fn argument<'f>(field: &'f ast::Field, name: &str) -> Option<&'f AstValue> {
    field
        .arguments
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.value.as_ref())
}

fn paged_field(
    field: &ast::Field,
    first: &AstValue,
    variables: &Map<String, Value>,
    mode: PageMode,
) -> Result<PagedField, String> {
    let key = field.alias.as_ref().unwrap_or(&field.name).to_string();
    let first = match first {
        AstValue::Int(int) => int.try_to_i32().ok().and_then(|n| u64::try_from(n).ok()),
        AstValue::Variable(name) => variables.get(name.as_str()).and_then(Value::as_u64),
        _ => None,
    }
    .filter(|n| *n > 0)
    .ok_or_else(|| format!("`{key}` needs a positive `first`"))?;
    let cursor = match mode {
        PageMode::IdGt => id_gt_variable(field),
        PageMode::Skip => match argument(field, "skip") {
            Some(AstValue::Variable(name)) => Some(name.to_string()),
            _ => None,
        },
    };
    let cursor = cursor.ok_or_else(|| match mode {
        PageMode::IdGt => format!("`{key}` needs `where: {{id_gt: $variable}}`"),
        PageMode::Skip => format!("`{key}` needs `skip: $variable`"),
    })?;
    Ok(PagedField { key, first, cursor })
}

fn id_gt_variable(field: &ast::Field) -> Option<String> {
    let Some(AstValue::Object(filters)) = argument(field, "where") else {
        return None;
    };
    filters
        .iter()
        .find(|(name, _)| name == "id_gt")
        .and_then(|(_, value)| match value.as_ref() {
            AstValue::Variable(name) => Some(name.to_string()),
            _ => None,
        })
}

/// The state of a paginated run: the items so far per field and the
/// variables of the next page.
#[derive(Debug)]
pub struct Pager {
    fields: Vec<PagedField>,
    mode: PageMode,
    /// Items so far, per field.
    items: Vec<Vec<Value>>,
    /// Fields whose last page was short.
    done: Vec<bool>,
    pages: u32,
    /// Body bytes of the pages taken.
    bytes: u64,
}

impl Pager {
    pub fn new(fields: Vec<PagedField>, mode: PageMode) -> Self {
        let n = fields.len();
        Self {
            fields,
            mode,
            items: vec![Vec::new(); n],
            done: vec![false; n],
            pages: 0,
            bytes: 0,
        }
    }

    /// Give each cursor its start (`""` or `0`) unless the request set it.
    pub fn start(&self, variables: &mut Map<String, Value>) {
        for field in &self.fields {
            let start = match self.mode {
                PageMode::IdGt => Value::String(String::new()),
                PageMode::Skip => Value::from(0),
            };
            variables.entry(field.cursor.clone()).or_insert(start);
        }
    }

    /// Take the page `data` (a body of `bytes`) and move the cursors of
    /// `variables` on. `Ok(true)` when another page is needed.
    pub fn take(
        &mut self,
        data: &Value,
        bytes: u64,
        variables: &mut Map<String, Value>,
    ) -> Result<bool, String> {
        // Check the whole page before keeping any of it.
        let mut pages = Vec::with_capacity(self.fields.len());
        let mut cursors = Map::new();
        for (field, done) in self.fields.iter().zip(&self.done) {
            if *done {
                pages.push(None);
                continue;
            }
            let Some(page) = data.get(&field.key).and_then(Value::as_array) else {
                return Err(format!("`{}` did not return a list", field.key));
            };
            if page.len() as u64 >= field.first {
                let next = next_cursor(self.mode, field, page, variables)?;
                cursors.insert(field.cursor.clone(), next);
            }
            pages.push(Some(page));
        }
        self.pages += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        for (i, page) in pages.into_iter().enumerate() {
            let Some(page) = page else { continue };
            self.items[i].extend(page.iter().cloned());
            self.done[i] = (page.len() as u64) < self.fields[i].first;
        }
        variables.extend(cursors);
        Ok(self.done.iter().any(|done| !done))
    }

    /// How many pages were taken.
    pub fn pages(&self) -> u32 {
        self.pages
    }

    /// Why paging stops although `more` pages remain: `--max-pages` or
    /// [`MAX_PAGED_BYTES`]. `None` to go on.
    pub fn stop_warning(&self, more: bool, max_pages: u32) -> Option<String> {
        if !more {
            return None;
        }
        if self.pages >= max_pages {
            return Some(capped_warning(max_pages));
        }
        (self.bytes >= MAX_PAGED_BYTES).then(|| bytes_capped_warning(self.pages))
    }

    /// `data` of the last page with every paginated field replaced by all
    /// its items.
    pub fn merge(self, mut data: Value) -> Value {
        if let Value::Object(object) = &mut data {
            for (field, items) in self.fields.into_iter().zip(self.items) {
                object.insert(field.key, Value::Array(items));
            }
        }
        data
    }
}

fn next_cursor(
    mode: PageMode,
    field: &PagedField,
    page: &[Value],
    variables: &Map<String, Value>,
) -> Result<Value, String> {
    match mode {
        PageMode::IdGt => page
            .last()
            .and_then(|item| item.get("id"))
            .filter(|id| id.is_string())
            .cloned()
            .ok_or_else(|| format!("`{}` items need an `id` for id_gt", field.key)),
        PageMode::Skip => {
            let skip = variables.get(&field.cursor).and_then(Value::as_u64);
            Ok(Value::from(skip.unwrap_or(0) + field.first))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ID_GT: &str =
        "query($last: String) { transfers(first: 2, where: {id_gt: $last}) { id } }";

    fn vars(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn plans_id_gt_and_skip_fields() {
        let fields = plan(ID_GT, None, &Map::new(), PageMode::IdGt).unwrap();
        assert_eq!(
            fields,
            vec![PagedField {
                key: "transfers".into(),
                first: 2,
                cursor: "last".into()
            }]
        );
        let skip = "query($n: Int, $s: Int) { all: tokens(first: $n, skip: $s) { id } }";
        let fields = plan(skip, None, &vars(json!({"n": 5})), PageMode::Skip).unwrap();
        assert_eq!((fields[0].key.as_str(), fields[0].first), ("all", 5));
        assert_eq!(fields[0].cursor, "s");
    }

    #[test]
    fn unpageable_operations_are_refused() {
        let none = plan("{ tokens { id } }", None, &Map::new(), PageMode::IdGt);
        assert_eq!(none.unwrap_err(), "no top-level field takes `first`");
        let no_cursor = plan(
            "{ tokens(first: 10) { id } }",
            None,
            &Map::new(),
            PageMode::IdGt,
        );
        assert_eq!(
            no_cursor.unwrap_err(),
            "`tokens` needs `where: {id_gt: $variable}`"
        );
        let skip = plan(
            "{ tokens(first: 10) { id } }",
            None,
            &Map::new(),
            PageMode::Skip,
        );
        assert_eq!(skip.unwrap_err(), "`tokens` needs `skip: $variable`");
    }

    #[test]
    fn pages_until_a_short_page() {
        let fields = plan(ID_GT, None, &Map::new(), PageMode::IdGt).unwrap();
        let mut pager = Pager::new(fields, PageMode::IdGt);
        let mut variables = Map::new();
        pager.start(&mut variables);
        assert_eq!(variables["last"], json!(""));
        let page = json!({"transfers": [{"id": "a"}, {"id": "b"}]});
        assert!(pager.take(&page, 10, &mut variables).unwrap());
        assert_eq!(variables["last"], json!("b"));
        let last = json!({"transfers": [{"id": "c"}]});
        assert!(!pager.take(&last, 10, &mut variables).unwrap());
        assert_eq!(pager.pages(), 2);
        let merged = pager.merge(last);
        assert_eq!(
            merged,
            json!({"transfers": [{"id": "a"}, {"id": "b"}, {"id": "c"}]})
        );
    }

    #[test]
    fn skip_moves_by_first() {
        let doc = "query($s: Int) { t: tokens(first: 2, skip: $s) { id } }";
        let fields = plan(doc, None, &Map::new(), PageMode::Skip).unwrap();
        let mut pager = Pager::new(fields, PageMode::Skip);
        let mut variables = Map::new();
        pager.start(&mut variables);
        let page = json!({"t": [{"id": "a"}, {"id": "b"}]});
        assert!(pager.take(&page, 10, &mut variables).unwrap());
        assert_eq!(variables["s"], json!(2));
        assert!(pager.take(&json!({"t": "x"}), 1, &mut variables).is_err());
        assert_eq!(PageMode::parse("skip"), Some(PageMode::Skip));
        assert_eq!(PageMode::parse("cursor"), None);
        assert_eq!(capped_warning(20), "stopped at --max-pages 20");
    }

    #[test]
    fn paging_stops_at_the_byte_limit() {
        let fields = plan(ID_GT, None, &Map::new(), PageMode::IdGt).unwrap();
        let mut pager = Pager::new(fields, PageMode::IdGt);
        let mut variables = Map::new();
        pager.start(&mut variables);
        let page = json!({"transfers": [{"id": "a"}, {"id": "b"}]});
        let half = MAX_PAGED_BYTES / 2;
        assert!(pager.take(&page, half, &mut variables).unwrap());
        assert_eq!(pager.stop_warning(true, 20), None);
        assert!(pager.take(&page, half, &mut variables).unwrap());
        assert_eq!(
            pager.stop_warning(true, 20).as_deref(),
            Some("stopped after 2 pages: they reached the 64 MiB pagination limit")
        );
        assert_eq!(pager.stop_warning(false, 20), None);
        assert_eq!(
            pager.stop_warning(true, 2).as_deref(),
            Some("stopped at --max-pages 2")
        );
    }

    #[test]
    fn partial_errors_count_the_kept_pages() {
        assert_eq!(
            partial_error(3, "HTTP 502"),
            "--paginate: page 3 failed (HTTP 502); keeping the 2 pages read before it"
        );
        assert!(partial_error(2, "x").contains("the 1 page read"));
    }
}
