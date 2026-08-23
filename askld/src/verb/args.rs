//! The argument list of one verb, with typed accessors.
//!
//! Every verb constructor takes an `&Args` instead of a
//! `(&Vec<Value>, &HashMap<String, Value>)` pair.  Two things follow from
//! that:
//!
//! 1. **One place decides what a type error reads like.**  `Args` knows the
//!    verb's name and which slot is being read, so it can say
//!    `search: 'limit' expects an integer, found the string "500" — write
//!    limit=500` without every call site threading that context by hand.
//! 2. **The old spelling teaches its replacement.**  Before arguments were
//!    typed, every value was quoted.  [`Value::unquoted_spelling`] recognises
//!    a quoted string that spells a value of the wanted type, and the error
//!    carries the fix.
//!
//! `Args` owns its values rather than borrowing them.  The synthetic call
//! sites — the `@x` / `@@x` / `#x` shortcuts and bare selectors in
//! [`crate::verb::construct_verb`] — build their argument lists on the fly,
//! and owning removes the lifetime plumbing that would otherwise force each
//! of them to bind a local `Vec` and `HashMap` first.  The cost is one clone
//! of already-parsed values per verb, at parse time.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::parser::{Value, ValueType};

/// Which argument of a verb is being read.  Selects both how the slot is
/// named in an error and how the migration hint spells the replacement.
#[derive(Debug, Clone, Copy)]
enum Slot<'a> {
    Named(&'a str),
    /// `index` is 0-based; errors report it 1-based, the way a reader counts
    /// arguments.  `what` names the argument (`line number`), since a
    /// positional has no keyword to identify it by.
    Positional {
        index: usize,
        what: &'a str,
    },
}

impl Slot<'_> {
    fn describe(&self) -> String {
        match self {
            Slot::Named(key) => format!("'{}'", key),
            Slot::Positional { index, what } => format!("argument {} ({})", index + 1, what),
        }
    }

    /// How to write `literal` in this slot.
    fn respell(&self, literal: &str) -> String {
        match self {
            Slot::Named(key) => format!("write {}={}", key, literal),
            Slot::Positional { .. } => format!("write it unquoted: {}", literal),
        }
    }
}

/// The parsed arguments of one verb.
#[derive(Debug, Clone)]
pub(crate) struct Args {
    verb: String,
    positional: Vec<Value>,
    named: HashMap<String, Value>,
}

impl Args {
    pub(crate) fn new(
        verb: impl Into<String>,
        positional: Vec<Value>,
        named: HashMap<String, Value>,
    ) -> Args {
        Args {
            verb: verb.into(),
            positional,
            named,
        }
    }

    /// An empty argument list, to be filled with [`Args::with`] /
    /// [`Args::with_named`].  For verbs synthesised from shortcuts.
    pub(crate) fn of(verb: impl Into<String>) -> Args {
        Args::new(verb, Vec::new(), HashMap::new())
    }

    pub(crate) fn with(mut self, value: Value) -> Args {
        self.positional.push(value);
        self
    }

    pub(crate) fn with_named(mut self, key: impl Into<String>, value: Value) -> Args {
        self.named.insert(key.into(), value);
        self
    }

    /// Number of positional arguments.
    pub(crate) fn count(&self) -> usize {
        self.positional.len()
    }

    pub(crate) fn no_positional(&self) -> bool {
        self.positional.is_empty()
    }

    pub(crate) fn has_named(&self, key: &str) -> bool {
        self.named.contains_key(key)
    }

    // === Raw access ===
    //
    // For arguments whose type is richer than one primitive — a name pattern
    // is a plain string OR a `g"..."` glob, a symbol reference is an integer
    // id OR an `"@label"` — the verb reads the `Value` and discriminates.

    pub(crate) fn value_at(&self, index: usize) -> Option<&Value> {
        self.positional.get(index)
    }

    pub(crate) fn named_value(&self, key: &str) -> Option<&Value> {
        self.named.get(key)
    }

    // === Positional ===

    pub(crate) fn str_at(&self, index: usize, what: &str) -> Result<&str> {
        let value = self.required_at(index, what)?;
        self.as_str(Slot::Positional { index, what }, value)
    }

    pub(crate) fn usize_at(&self, index: usize, what: &str) -> Result<usize> {
        let value = self.required_at(index, what)?;
        let slot = Slot::Positional { index, what };
        self.as_usize(slot, value)
    }

    fn required_at(&self, index: usize, what: &str) -> Result<&Value> {
        self.positional.get(index).ok_or_else(|| {
            anyhow::anyhow!("{}: missing argument {} ({})", self.verb, index + 1, what,)
        })
    }

    // === Named, optional ===

    pub(crate) fn named_str(&self, key: &str) -> Result<Option<&str>> {
        match self.named.get(key) {
            Some(value) => self.as_str(Slot::Named(key), value).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn named_bool(&self, key: &str) -> Result<Option<bool>> {
        match self.named.get(key) {
            Some(value) => self.as_bool(Slot::Named(key), value).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn named_i32(&self, key: &str) -> Result<Option<i32>> {
        match self.named.get(key) {
            Some(value) => self.as_i32(Slot::Named(key), value).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn named_usize(&self, key: &str) -> Result<Option<usize>> {
        match self.named.get(key) {
            Some(value) => self.as_usize(Slot::Named(key), value).map(Some),
            None => Ok(None),
        }
    }

    // === Named, required ===

    pub(crate) fn req_str(&self, key: &str) -> Result<&str> {
        self.as_str(Slot::Named(key), self.required_named(key)?)
    }

    pub(crate) fn req_i64(&self, key: &str) -> Result<i64> {
        self.as_i64(Slot::Named(key), self.required_named(key)?)
    }

    pub(crate) fn req_i32(&self, key: &str) -> Result<i32> {
        self.as_i32(Slot::Named(key), self.required_named(key)?)
    }

    /// A required named argument as a raw [`Value`].  For arguments whose
    /// type is a union — `symbol_id` is an integer id or an `"@label"`.
    pub(crate) fn required_named(&self, key: &str) -> Result<&Value> {
        self.named
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("{}: requires a '{}' argument", self.verb, key))
    }

    // === Whole-list checks ===

    /// Reject named arguments the verb does not understand, so a typo
    /// surfaces at parse time instead of being silently ignored.
    pub(crate) fn allow(&self, keys: &[&str]) -> Result<()> {
        for key in self.named.keys() {
            if !keys.contains(&key.as_str()) {
                if keys.is_empty() {
                    bail!("{}: takes no named arguments, found '{}'", self.verb, key);
                }
                bail!(
                    "{}: unknown argument '{}' (allowed: {})",
                    self.verb,
                    key,
                    keys.join(", ")
                );
            }
        }
        Ok(())
    }

    // === Conversions ===

    fn as_str<'a>(&self, slot: Slot<'_>, value: &'a Value) -> Result<&'a str> {
        value
            .as_plain()
            .map_err(|_| self.type_error(slot, ValueType::Str, value))
    }

    fn as_bool(&self, slot: Slot<'_>, value: &Value) -> Result<bool> {
        match value {
            Value::Bool(b) => Ok(*b),
            other => Err(self.type_error(slot, ValueType::Bool, other)),
        }
    }

    fn as_i64(&self, slot: Slot<'_>, value: &Value) -> Result<i64> {
        match value {
            Value::Int(n) => Ok(*n),
            other => Err(self.type_error(slot, ValueType::Int, other)),
        }
    }

    fn as_i32(&self, slot: Slot<'_>, value: &Value) -> Result<i32> {
        let n = self.as_i64(slot, value)?;
        i32::try_from(n).map_err(|_| {
            anyhow::anyhow!(
                "{}: {} must fit in a 32-bit integer, got {}",
                self.verb,
                slot.describe(),
                n
            )
        })
    }

    fn as_usize(&self, slot: Slot<'_>, value: &Value) -> Result<usize> {
        let n = self.as_i64(slot, value)?;
        usize::try_from(n).map_err(|_| {
            anyhow::anyhow!(
                "{}: {} must not be negative, got {}",
                self.verb,
                slot.describe(),
                n
            )
        })
    }

    fn type_error(&self, slot: Slot<'_>, want: ValueType, found: &Value) -> anyhow::Error {
        let hint = match found.unquoted_spelling(want) {
            Some(literal) => format!(" — {}", slot.respell(literal)),
            None => String::new(),
        };
        anyhow::anyhow!(
            "{}: {} expects {}, found {}{}",
            self.verb,
            slot.describe(),
            want.describe(),
            found.describe(),
            hint
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args::of("search")
            .with(Value::plain("needle"))
            .with_named("limit", Value::Int(500))
            .with_named("whole_word", Value::Bool(true))
    }

    #[test]
    fn typed_values_read_back() {
        let args = args();
        assert_eq!(args.str_at(0, "query").unwrap(), "needle");
        assert_eq!(args.named_usize("limit").unwrap(), Some(500));
        assert_eq!(args.named_bool("whole_word").unwrap(), Some(true));
        assert_eq!(args.named_bool("case").unwrap(), None);
    }

    #[test]
    fn the_quoted_spelling_teaches_its_replacement() {
        let args = Args::of("search")
            .with_named("limit", Value::plain("500"))
            .with_named("whole_word", Value::plain("true"));

        let msg = args.named_usize("limit").unwrap_err().to_string();
        assert_eq!(
            msg,
            "search: 'limit' expects an integer, found the string \"500\" — write limit=500"
        );

        let msg = args.named_bool("whole_word").unwrap_err().to_string();
        assert_eq!(
            msg,
            "search: 'whole_word' expects a boolean (true or false), \
             found the string \"true\" — write whole_word=true"
        );
    }

    #[test]
    fn a_positional_hint_does_not_invent_a_keyword() {
        let args = Args::of("loc")
            .with(Value::plain("read_write.c"))
            .with(Value::plain("42"));
        let msg = args.usize_at(1, "line number").unwrap_err().to_string();
        assert_eq!(
            msg,
            "loc: argument 2 (line number) expects an integer, \
             found the string \"42\" — write it unquoted: 42"
        );
    }

    #[test]
    fn a_string_that_spells_nothing_gets_no_hint() {
        // `inherit="yes"` was silently false before arguments were typed.
        // There is no requoting that fixes it, so the error does not pretend
        // there is — it just says what was expected.
        let args = Args::of("func").with_named("inherit", Value::plain("yes"));
        let msg = args.named_bool("inherit").unwrap_err().to_string();
        assert_eq!(
            msg,
            "func: 'inherit' expects a boolean (true or false), found the string \"yes\""
        );
    }

    #[test]
    fn range_checks_name_the_slot() {
        let args = Args::of("ephemeral_symbol").with_named("project_id", Value::Int(1 << 40));
        let msg = args.named_i32("project_id").unwrap_err().to_string();
        assert!(
            msg.contains("'project_id' must fit in a 32-bit integer"),
            "got: {msg}"
        );

        let args = Args::of("search").with_named("limit", Value::Int(-1));
        let msg = args.named_usize("limit").unwrap_err().to_string();
        assert!(msg.contains("'limit' must not be negative"), "got: {msg}");
    }

    #[test]
    fn unknown_named_arguments_are_rejected() {
        let args = Args::of("search").with_named("limt", Value::Int(5));
        let msg = args
            .allow(&["case", "whole_word", "limit"])
            .unwrap_err()
            .to_string();
        assert_eq!(
            msg,
            "search: unknown argument 'limt' (allowed: case, whole_word, limit)"
        );
    }

    #[test]
    fn a_missing_required_argument_names_what_is_missing() {
        let args = Args::of("loc").with(Value::plain("read_write.c"));
        assert_eq!(
            args.usize_at(1, "line number").unwrap_err().to_string(),
            "loc: missing argument 2 (line number)"
        );
        assert_eq!(
            Args::of("ephemeral_symbol")
                .req_str("name")
                .unwrap_err()
                .to_string(),
            "ephemeral_symbol: requires a 'name' argument"
        );
    }
}
