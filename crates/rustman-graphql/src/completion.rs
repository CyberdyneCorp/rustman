//! Context-aware completion for a document being typed.
//!
//! The document is read token by token up to the cursor with
//! `apollo-parser`'s lexer, which never fails: a half-typed document still
//! yields the enclosing selection set, argument list or value position.
//! Candidates then come from the typed schema.

use apollo_compiler::ast::{InputValueDefinition, OperationType};
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::{Node, Schema};
use apollo_parser::{Lexer, TokenKind};
use serde::Serialize;

use crate::explorer::{directive, field_definitions, field_info, input_value};
use crate::schema::GraphqlSchema;

/// What a candidate completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    Field,
    Argument,
    EnumValue,
    Variable,
    Directive,
    Fragment,
}

/// One completion candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Completion {
    /// The text to insert (without `$`, `@` or `...`).
    pub label: String,
    pub kind: CompletionKind,
    /// The type signature, such as `user(id: ID!): User` or `ID!`.
    pub detail: String,
}

/// The candidates at byte offset `cursor` of `document`, filtered by the
/// name being typed there.
pub fn complete(schema: &GraphqlSchema, document: &str, cursor: usize) -> Vec<Completion> {
    let tokens = significant_tokens(document);
    let cursor = cursor.min(document.len());
    let (before, prefix) = split_at_cursor(&tokens, cursor);
    let mut scanner = Scanner::new(schema.schema());
    for token in before {
        scanner.token(token);
    }
    let fragments = fragment_names(&tokens);
    let mut candidates = scanner.candidates(&fragments);
    candidates.retain(|c| c.label.starts_with(prefix));
    candidates
}

/// A token: its kind, text and byte offset.
#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    kind: TokenKind,
    text: &'a str,
    offset: usize,
}

fn significant_tokens(document: &str) -> Vec<Token<'_>> {
    Lexer::new(document)
        .filter_map(Result::ok)
        .filter(|t| {
            !matches!(
                t.kind(),
                TokenKind::Whitespace | TokenKind::Comment | TokenKind::Comma | TokenKind::Eof
            )
        })
        .map(|t| Token {
            kind: t.kind(),
            text: t.data(),
            offset: t.index(),
        })
        .collect()
}

/// The tokens before the cursor, and the name being typed at it.
fn split_at_cursor<'t, 'a>(tokens: &'t [Token<'a>], cursor: usize) -> (&'t [Token<'a>], &'a str) {
    let before = tokens.partition_point(|t| t.offset < cursor);
    match before.checked_sub(1).map(|i| tokens[i]) {
        Some(last) if last.kind == TokenKind::Name && last.offset + last.text.len() >= cursor => {
            let typed = &last.text[..cursor - last.offset];
            (&tokens[..before - 1], typed)
        }
        _ => (&tokens[..before], ""),
    }
}

/// Every `fragment Name on Type` of the document, with its type.
fn fragment_names(tokens: &[Token<'_>]) -> Vec<(String, String)> {
    tokens
        .windows(4)
        .filter(|w| {
            w[0].kind == TokenKind::Name
                && w[0].text == "fragment"
                && w[1].kind == TokenKind::Name
                && w[2].text == "on"
                && w[3].kind == TokenKind::Name
        })
        .map(|w| (w[1].text.to_owned(), w[3].text.to_owned()))
        .collect()
}

/// What an argument list belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Owner {
    Field { parent: String, name: String },
    Directive(String),
}

/// An open bracket.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    /// `{ ... }` selecting from the type, when known.
    Selection(Option<String>),
    /// `( ... )` of a field or directive; the argument being written.
    Arguments { owner: Owner, argument: String },
    /// `( ... )` of an operation's variable definitions.
    VariableDefinitions,
    /// A list or input object value; the enum its items take, if any.
    Value(Option<String>),
}

/// What the last significant token was.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Prev {
    Start,
    Dollar,
    At,
    Spread,
    Colon,
    On,
    Field(String),
    Directive(String),
    OperationKeyword,
    FragmentKeyword,
    /// An operation or fragment name, or a type condition.
    Header,
    Other,
}

struct Scanner<'s> {
    schema: &'s Schema,
    scopes: Vec<Scope>,
    prev: Prev,
    /// The type the next `{` selects from.
    pending_type: Option<String>,
    /// Variables declared by the current operation, with their types.
    variables: Vec<(String, String)>,
    /// Collecting the type of the last declared variable.
    typing_variable: bool,
}

impl<'s> Scanner<'s> {
    fn new(schema: &'s Schema) -> Self {
        Self {
            schema,
            scopes: Vec::new(),
            prev: Prev::Start,
            pending_type: None,
            variables: Vec::new(),
            typing_variable: false,
        }
    }

    fn token(&mut self, token: &Token<'_>) {
        self.prev = match token.kind {
            TokenKind::Name => self.name(token.text),
            TokenKind::Dollar => Prev::Dollar,
            TokenKind::At => Prev::At,
            TokenKind::Spread => Prev::Spread,
            TokenKind::Colon => self.colon(),
            TokenKind::LCurly => self.open_curly(),
            TokenKind::LParen => self.open_paren(),
            TokenKind::LBracket => self.open_bracket(),
            TokenKind::RCurly | TokenKind::RParen | TokenKind::RBracket => self.close(token.kind),
            TokenKind::Bang | TokenKind::Eq => self.variable_punctuation(token),
            _ => Prev::Other,
        };
    }

    fn name(&mut self, text: &str) -> Prev {
        match (&self.prev, self.scopes.last()) {
            (Prev::Dollar, _) => self.variable(text),
            (Prev::At, _) => Prev::Directive(text.to_owned()),
            (Prev::Spread | Prev::Header, _) if text == "on" => Prev::On,
            (Prev::On, _) => {
                self.pending_type = Some(text.to_owned());
                Prev::Header
            }
            (_, None) => self.top_level_name(text),
            (_, Some(Scope::Selection(parent))) => {
                let parent = parent.clone();
                self.field_name(parent.as_deref(), text)
            }
            (_, Some(Scope::VariableDefinitions)) => self.variable_type(text),
            (prev, Some(Scope::Arguments { .. })) if *prev != Prev::Colon => {
                self.set_argument(text);
                Prev::Other
            }
            _ => Prev::Other,
        }
    }

    fn top_level_name(&mut self, text: &str) -> Prev {
        let root = match text {
            "query" => OperationType::Query,
            "mutation" => OperationType::Mutation,
            "subscription" => OperationType::Subscription,
            "fragment" => {
                self.variables.clear();
                return Prev::FragmentKeyword;
            }
            _ => return Prev::Header,
        };
        self.variables.clear();
        self.pending_type = self.schema.root_operation(root).map(|n| n.to_string());
        Prev::OperationKeyword
    }

    fn field_name(&mut self, parent: Option<&str>, name: &str) -> Prev {
        let field = parent.and_then(|p| self.schema.type_field(p, name).ok());
        self.pending_type = field.map(|f| f.ty.inner_named_type().to_string());
        Prev::Field(name.to_owned())
    }

    fn variable(&mut self, name: &str) -> Prev {
        if self.scopes.last() == Some(&Scope::VariableDefinitions) {
            self.variables.push((name.to_owned(), String::new()));
        }
        Prev::Other
    }

    fn variable_type(&mut self, text: &str) -> Prev {
        if self.typing_variable {
            self.push_type_text(text);
        }
        Prev::Other
    }

    fn variable_punctuation(&mut self, token: &Token<'_>) -> Prev {
        if token.kind == TokenKind::Eq {
            self.typing_variable = false;
        } else if self.typing_variable {
            self.push_type_text(token.text);
        }
        Prev::Other
    }

    fn push_type_text(&mut self, text: &str) {
        if let Some((_, ty)) = self.variables.last_mut() {
            ty.push_str(text);
        }
    }

    fn colon(&mut self) -> Prev {
        if self.scopes.last() == Some(&Scope::VariableDefinitions) {
            self.typing_variable = true;
            return Prev::Other;
        }
        Prev::Colon
    }

    fn set_argument(&mut self, text: &str) {
        if let Some(Scope::Arguments { argument, .. }) = self.scopes.last_mut() {
            *argument = text.to_owned();
        }
    }

    fn open_curly(&mut self) -> Prev {
        let scope = match self.scopes.last() {
            None => {
                let query = || self.schema.root_operation(OperationType::Query);
                let ty = self
                    .pending_type
                    .take()
                    .or_else(|| query().map(|n| n.to_string()));
                Scope::Selection(ty)
            }
            Some(Scope::Selection(_)) => Scope::Selection(self.pending_type.take()),
            Some(_) => Scope::Value(None),
        };
        self.scopes.push(scope);
        Prev::Other
    }

    fn open_paren(&mut self) -> Prev {
        let scope = match (&self.prev, self.scopes.last()) {
            (Prev::Directive(name), _) => Scope::Arguments {
                owner: Owner::Directive(name.clone()),
                argument: String::new(),
            },
            (Prev::Field(name), Some(Scope::Selection(Some(parent)))) => Scope::Arguments {
                owner: Owner::Field {
                    parent: parent.clone(),
                    name: name.clone(),
                },
                argument: String::new(),
            },
            (_, None) => Scope::VariableDefinitions,
            _ => Scope::Value(None),
        };
        self.scopes.push(scope);
        Prev::Other
    }

    fn open_bracket(&mut self) -> Prev {
        match self.scopes.last() {
            Some(Scope::VariableDefinitions) => {
                if self.typing_variable {
                    self.push_type_text("[");
                }
            }
            Some(Scope::Arguments { .. } | Scope::Value(_)) => {
                let items = self.value_enum();
                self.scopes.push(Scope::Value(items));
            }
            _ => {}
        }
        Prev::Other
    }

    fn close(&mut self, kind: TokenKind) -> Prev {
        if kind == TokenKind::RBracket && self.scopes.last() == Some(&Scope::VariableDefinitions) {
            if self.typing_variable {
                self.push_type_text("]");
            }
            return Prev::Other;
        }
        if kind == TokenKind::RParen {
            self.typing_variable = false;
        }
        self.scopes.pop();
        if kind == TokenKind::RCurly && self.scopes.is_empty() {
            self.variables.clear();
        }
        Prev::Other
    }

    /// The enum type of the value being written, if it is one.
    fn value_enum(&self) -> Option<String> {
        match self.scopes.last()? {
            Scope::Value(items) => items.clone(),
            Scope::Arguments { owner, argument } => {
                let definition = self.argument_definitions(owner)?;
                let ty = definition
                    .iter()
                    .find(|a| a.name == argument.as_str())?
                    .ty
                    .clone();
                let named = ty.inner_named_type();
                matches!(self.schema.types.get(named), Some(ExtendedType::Enum(_)))
                    .then(|| named.to_string())
            }
            _ => None,
        }
    }

    fn argument_definitions(&self, owner: &Owner) -> Option<&'s [Node<InputValueDefinition>]> {
        match owner {
            Owner::Field { parent, name } => {
                let field = self.schema.type_field(parent, name).ok()?;
                Some(&field.arguments)
            }
            Owner::Directive(name) => Some(
                &self
                    .schema
                    .directive_definitions
                    .get(name.as_str())?
                    .arguments,
            ),
        }
    }

    fn candidates(&self, fragments: &[(String, String)]) -> Vec<Completion> {
        match (&self.prev, self.scopes.last()) {
            (Prev::Dollar, _) => self.variable_candidates(),
            (Prev::At, _) => self.directive_candidates(),
            (Prev::Spread, _) => fragment_candidates(fragments),
            (Prev::Colon, Some(Scope::Arguments { .. })) => self.enum_candidates(),
            (_, Some(Scope::Value(Some(_)))) => self.enum_candidates(),
            (_, Some(Scope::Arguments { owner, .. })) => self.argument_candidates(owner),
            (_, Some(Scope::Selection(Some(ty)))) => self.field_candidates(ty),
            _ => Vec::new(),
        }
    }

    fn field_candidates(&self, ty: &str) -> Vec<Completion> {
        let Some(definition) = self.schema.types.get(ty) else {
            return Vec::new();
        };
        let mut found: Vec<Completion> = field_definitions(definition)
            .into_iter()
            .flat_map(|fields| fields.values())
            .map(|f| Completion {
                label: f.name.to_string(),
                kind: CompletionKind::Field,
                detail: field_info(f).signature(),
            })
            .collect();
        found.push(Completion {
            label: "__typename".to_owned(),
            kind: CompletionKind::Field,
            detail: "__typename: String!".to_owned(),
        });
        found
    }

    fn argument_candidates(&self, owner: &Owner) -> Vec<Completion> {
        self.argument_definitions(owner)
            .unwrap_or_default()
            .iter()
            .map(|a| {
                let info = input_value(a);
                Completion {
                    label: info.name.clone(),
                    kind: CompletionKind::Argument,
                    detail: info.signature(),
                }
            })
            .collect()
    }

    fn enum_candidates(&self) -> Vec<Completion> {
        let Some(name) = self.value_enum() else {
            return Vec::new();
        };
        let Some(ExtendedType::Enum(enum_type)) = self.schema.types.get(name.as_str()) else {
            return Vec::new();
        };
        enum_type
            .values
            .keys()
            .map(|value| Completion {
                label: value.to_string(),
                kind: CompletionKind::EnumValue,
                detail: name.clone(),
            })
            .collect()
    }

    fn variable_candidates(&self) -> Vec<Completion> {
        self.variables
            .iter()
            .map(|(name, ty)| Completion {
                label: name.clone(),
                kind: CompletionKind::Variable,
                detail: ty.clone(),
            })
            .collect()
    }

    fn directive_candidates(&self) -> Vec<Completion> {
        self.schema
            .directive_definitions
            .values()
            .map(|d| {
                let info = directive(d);
                let args: Vec<String> = info.arguments.iter().map(|a| a.signature()).collect();
                let detail = match args.is_empty() {
                    true => format!("@{}", info.name),
                    false => format!("@{}({})", info.name, args.join(", ")),
                };
                Completion {
                    label: info.name,
                    kind: CompletionKind::Directive,
                    detail,
                }
            })
            .collect()
    }
}

fn fragment_candidates(fragments: &[(String, String)]) -> Vec<Completion> {
    fragments
        .iter()
        .map(|(name, on)| Completion {
            label: name.clone(),
            kind: CompletionKind::Fragment,
            detail: format!("on {on}"),
        })
        .collect()
}
