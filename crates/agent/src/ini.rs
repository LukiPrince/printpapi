// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! A minimal INI reader with the parts of Python's `configparser` that agent.ini relies on:
//! `[sections]`, `key = value` / `key: value`, full-line `;`/`#` comments (no inline comments, so
//! `printers = A ; B` keeps its `;`), lowercased keys, indented continuation lines joined with
//! `\n`, a `[DEFAULT]` fallback section, strict duplicate detection, and `BasicInterpolation`.

use std::collections::HashSet;

type Entries = Vec<(String, String)>;
type Pending = Vec<(String, Vec<String>)>;

#[derive(Debug, Default, Clone)]
pub struct Ini {
    defaults: Entries,
    sections: Vec<(String, Entries)>,
}

enum Cur {
    None,
    Defaults,
    Section(usize),
}

/// The option list the cursor points at.
fn cur_entries<'a>(
    cur: &Cur,
    defaults: &'a mut Pending,
    sections: &'a mut [(String, Pending)],
) -> Option<&'a mut Pending> {
    match cur {
        Cur::None => None,
        Cur::Defaults => Some(defaults),
        Cur::Section(i) => sections.get_mut(*i).map(|(_, e)| e),
    }
}

/// configparser's `\[(?P<header>.+)\]`: from the first `[` to the last `]`, at least one char.
fn section_header(clean: &str) -> Option<&str> {
    let inner = clean.strip_prefix('[')?;
    let end = inner.rfind(']')?;
    (end > 0).then(|| &inner[..end])
}

impl Ini {
    /// Parse `text`; every problem is collected like configparser's `ParsingError`.
    pub fn parse(text: &str) -> Result<Ini, String> {
        // Option values stay line lists until the end so continuation lines can append.
        let mut defaults: Pending = Vec::new();
        let mut sections: Vec<(String, Pending)> = Vec::new();
        let mut cur = Cur::None;
        let mut optname: Option<String> = None;
        let mut indent_level = 0usize;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut seen_sections: HashSet<String> = HashSet::new();
        let mut sectname = String::new();
        let mut errors: Vec<String> = Vec::new();

        for (i, line) in text.lines().enumerate() {
            let lineno = i + 1;
            let clean = line.trim();
            let is_comment = clean.starts_with(';') || clean.starts_with('#');
            if clean.is_empty() || is_comment {
                // A blank line (not a comment) belongs to a running multi-line value.
                if !is_comment
                    && optname.is_some()
                    && let Some(entries) = cur_entries(&cur, &mut defaults, &mut sections)
                    && let Some(last) = entries.last_mut()
                {
                    last.1.push(String::new());
                }
                continue;
            }
            let cur_indent = line.chars().take_while(|c| c.is_whitespace()).count();
            if !matches!(cur, Cur::None) && optname.is_some() && cur_indent > indent_level {
                if let Some(entries) = cur_entries(&cur, &mut defaults, &mut sections)
                    && let Some(last) = entries.last_mut()
                {
                    last.1.push(clean.to_string());
                }
                continue;
            }
            indent_level = cur_indent;
            if let Some(header) = section_header(clean) {
                sectname = header.to_string();
                if sectname == "DEFAULT" {
                    cur = Cur::Defaults;
                } else if let Some(idx) = sections.iter().position(|(n, _)| *n == sectname) {
                    if seen_sections.contains(&sectname) {
                        return Err(format!(
                            "While reading from agent.ini [line {lineno:2}]: section {sectname:?} already exists"
                        ));
                    }
                    cur = Cur::Section(idx);
                } else {
                    sections.push((sectname.clone(), Vec::new()));
                    cur = Cur::Section(sections.len() - 1);
                    seen_sections.insert(sectname.clone());
                }
                optname = None;
                continue;
            }
            if matches!(cur, Cur::None) {
                return Err(format!(
                    "File contains no section headers.\nagent.ini, line: {lineno}\n{line:?}"
                ));
            }
            let Some(pos) = clean.find(['=', ':']) else {
                errors.push(format!("[line {lineno:2}]: {line:?}"));
                continue;
            };
            let name = clean[..pos].trim_end();
            let value = clean[pos + 1..].trim();
            if name.is_empty() {
                errors.push(format!("[line {lineno:2}]: {line:?}"));
            }
            let name = name.to_lowercase();
            if !seen.insert((sectname.clone(), name.clone())) {
                return Err(format!(
                    "While reading from agent.ini [line {lineno:2}]: option {name:?} in section {sectname:?} already exists"
                ));
            }
            if let Some(entries) = cur_entries(&cur, &mut defaults, &mut sections) {
                entries.push((name.clone(), vec![value.to_string()]));
            }
            optname = Some(name);
        }
        if !errors.is_empty() {
            return Err(format!(
                "source contains parsing errors: agent.ini\n\t{}",
                errors.join("\n\t")
            ));
        }
        let join = |v: Vec<String>| v.join("\n").trim_end().to_string();
        let flatten = |e: Pending| {
            e.into_iter()
                .map(|(k, v)| (k, join(v)))
                .collect::<Entries>()
        };
        Ok(Ini {
            defaults: flatten(defaults),
            sections: sections.into_iter().map(|(n, e)| (n, flatten(e))).collect(),
        })
    }

    pub fn has_section(&self, name: &str) -> bool {
        self.sections.iter().any(|(n, _)| n == name)
    }

    /// The `[DEFAULT]` keys.
    pub fn defaults(&self) -> &[(String, String)] {
        &self.defaults
    }

    /// The keys of one section (without the `[DEFAULT]` fallbacks), in file order.
    pub fn section(&self, name: &str) -> Option<&[(String, String)]> {
        self.sections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, e)| e.as_slice())
    }

    /// `parser.items(section, raw=True)`: DEFAULT keys first, overridden by the section's own.
    pub fn items_raw(&self, section: &str) -> Option<Entries> {
        let own = self.section(section)?;
        let mut out: Entries = self.defaults.clone();
        for (k, v) in own {
            match out.iter_mut().find(|(ok, _)| ok == k) {
                Some(slot) => slot.1 = v.clone(),
                None => out.push((k.clone(), v.clone())),
            }
        }
        Some(out)
    }

    /// Raw (uninterpolated) lookup: the section, then `[DEFAULT]`.
    pub fn get_raw(&self, section: &str, key: &str) -> Option<&str> {
        let key = key.to_lowercase();
        let own = self.section(section)?.iter().find(|(k, _)| *k == key);
        own.or_else(|| self.defaults.iter().find(|(k, _)| *k == key))
            .map(|(_, v)| v.as_str())
    }

    /// `parser[section].get(key)` with `BasicInterpolation`: `%%` -> `%`, `%(key)s` from the same
    /// section or `[DEFAULT]`, any other `%` is an error.
    pub fn get(&self, section: &str, key: &str) -> Result<Option<String>, String> {
        let Some(raw) = self.get_raw(section, key) else {
            return Ok(None);
        };
        let mut out = String::new();
        self.interpolate(section, key, raw, &mut out, 1)?;
        Ok(Some(out))
    }

    fn interpolate(
        &self,
        section: &str,
        option: &str,
        value: &str,
        out: &mut String,
        depth: u32,
    ) -> Result<(), String> {
        if depth > 10 {
            return Err(format!(
                "Recursion limit exceeded in value substitution: option {option:?} in section {section:?} contains an interpolation key which cannot be substituted in 10 steps. Raw value: {value:?}"
            ));
        }
        let mut rest = value;
        while !rest.is_empty() {
            let Some(p) = rest.find('%') else {
                out.push_str(rest);
                return Ok(());
            };
            out.push_str(&rest[..p]);
            rest = &rest[p..];
            let after = &rest[1..];
            if after.starts_with('%') {
                out.push('%');
                rest = &rest[2..];
            } else if let Some(reference) = after.strip_prefix('(') {
                let parsed = reference
                    .split_once(')')
                    .filter(|(k, tail)| !k.is_empty() && tail.starts_with('s'));
                let Some((k, tail)) = parsed else {
                    return Err(format!("bad interpolation variable reference {rest:?}"));
                };
                let var = k.to_lowercase();
                let Some(v) = self.get_raw(section, &var) else {
                    return Err(format!(
                        "Bad value substitution: option {option:?} in section {section:?} contains an interpolation key {var:?} which is not a valid option name. Raw value: {value:?}"
                    ));
                };
                if v.contains('%') {
                    self.interpolate(section, option, v, out, depth + 1)?;
                } else {
                    out.push_str(v);
                }
                rest = &tail[1..];
            } else {
                return Err(format!(
                    "'%' must be followed by '%' or '(', found: {rest:?}"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_delimiters_and_lowercases_keys() {
        let ini = Ini::parse("[agent]\nServer_URL=http://x\nname: pc\n").unwrap();
        assert_eq!(
            ini.get("agent", "server_url").unwrap().as_deref(),
            Some("http://x")
        );
        assert_eq!(ini.get("agent", "NAME").unwrap().as_deref(), Some("pc"));
    }

    #[test]
    fn comments_are_full_line_only_so_printers_keep_their_semicolons() {
        let ini = Ini::parse("; hi\n# there\n[agent]\nprinters = A ; B\n  ; comment\n").unwrap();
        assert_eq!(
            ini.get("agent", "printers").unwrap().as_deref(),
            Some("A ; B")
        );
    }

    #[test]
    fn continuation_lines_join_with_newline_and_trailing_blanks_drop() {
        let ini = Ini::parse("[a]\nk = one\n  two\n\n  three\n\nx = y\n").unwrap();
        assert_eq!(
            ini.get("a", "k").unwrap().as_deref(),
            Some("one\ntwo\n\nthree")
        );
        assert_eq!(ini.get("a", "x").unwrap().as_deref(), Some("y"));
    }

    #[test]
    fn default_section_is_the_fallback() {
        let ini = Ini::parse("[DEFAULT]\ntimeout = 45\n[agent]\nk = v\n").unwrap();
        assert_eq!(ini.get("agent", "timeout").unwrap().as_deref(), Some("45"));
        assert_eq!(ini.get("agent", "missing").unwrap(), None);
    }

    #[test]
    fn interpolation_percent_escape_and_reference() {
        let ini = Ini::parse("[agent]\nbase = http://x\nurl = %(base)s/a\npct = 100%%\n").unwrap();
        assert_eq!(
            ini.get("agent", "url").unwrap().as_deref(),
            Some("http://x/a")
        );
        assert_eq!(ini.get("agent", "pct").unwrap().as_deref(), Some("100%"));
    }

    #[test]
    fn lone_percent_is_an_error_but_raw_reads_survive() {
        let ini = Ini::parse("[agent]\nk = a%b\n").unwrap();
        assert!(
            ini.get("agent", "k")
                .unwrap_err()
                .contains("'%' must be followed by")
        );
        assert_eq!(ini.get_raw("agent", "k"), Some("a%b"));
    }

    #[test]
    fn interpolation_loops_and_missing_keys_error() {
        let ini = Ini::parse("[a]\nx = %(y)s\ny = %(x)s\nz = %(nope)s\n").unwrap();
        assert!(ini.get("a", "x").unwrap_err().contains("Recursion limit"));
        assert!(
            ini.get("a", "z")
                .unwrap_err()
                .contains("not a valid option name")
        );
    }

    #[test]
    fn malformed_files_error() {
        assert!(Ini::parse("k = v\n").is_err()); // no section header
        assert!(Ini::parse("[a]\njust text\n").is_err()); // no delimiter
        assert!(Ini::parse("[a]\n= v\n").is_err()); // empty key
        assert!(Ini::parse("[a]\nk=1\nk=2\n").is_err()); // strict duplicates
        assert!(Ini::parse("[a]\n[a]\n").is_err());
    }

    #[test]
    fn items_raw_merges_defaults_under_the_section() {
        let ini = Ini::parse("[DEFAULT]\na = 1\nb = 2\n[s]\nb = 3\nc = 4\n").unwrap();
        let items = ini.items_raw("s").unwrap();
        assert_eq!(
            items,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "3".into()),
                ("c".into(), "4".into())
            ]
        );
        assert!(ini.items_raw("nope").is_none());
    }
}
