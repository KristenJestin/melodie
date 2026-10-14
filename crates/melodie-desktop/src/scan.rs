//! The window's sources, as the tests that refuse a pattern in them read them: comments left
//! out, and each line joined to the one before it, so that a call is found whole however rustfmt
//! laid it out.

use std::path::Path;

pub struct Source {
    pub name: String,
    code: String,
    /// The line of the file each byte of `code` comes from.
    lines: Vec<usize>,
}

impl Source {
    pub fn new(name: &str, text: &str) -> Self {
        let (mut code, mut lines) = (String::new(), Vec::new());
        for (n, line) in text.lines().enumerate() {
            let line = line.split("//").next().unwrap().trim_start();
            code.push_str(line);
            lines.extend(std::iter::repeat_n(n + 1, line.len()));
        }
        Source {
            name: name.into(),
            code,
            lines,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    /// The line of the file where `at`, an offset in `code`, was.
    pub fn line(&self, at: usize) -> usize {
        self.lines[at]
    }

    /// Each call to the function or method `name`: where it starts in `code`, and what is between
    /// its parentheses.
    pub fn calls(&self, name: &str) -> Vec<(usize, &str)> {
        let open = format!("{name}(");
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        self.code
            .match_indices(&open)
            .filter(|(at, _)| !self.code[..*at].ends_with(ident))
            .filter_map(|(at, _)| {
                let from = at + open.len();
                let mut depth = 1;
                let len = self.code[from..].find(|c| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                })?;
                Some((at, &self.code[from..from + len]))
            })
            .collect()
    }
}

/// Every source of the crate but `skip` (and this one, whose tests hold the patterns).
pub fn sources(skip: &str) -> Vec<Source> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    std::fs::read_dir(dir)
        .unwrap()
        .map(|f| f.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "rs")
                && !p.ends_with(skip)
                && !p.ends_with("scan.rs")
        })
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            Source::new(&name, &std::fs::read_to_string(&p).unwrap())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_split_over_lines_is_read_whole() {
        let s = Source::new(
            "a.rs",
            "div()\n    .child(\n        format!(\"{n}\"),\n    )\n",
        );
        assert_eq!(s.calls("child"), [(6, "format!(\"{n}\"),")]);
    }

    #[test]
    fn calls_inside_calls_are_found_too() {
        let s = Source::new("a.rs", "a.child(b.child(c))");
        let args: Vec<&str> = s.calls("child").into_iter().map(|(_, a)| a).collect();
        assert_eq!(args, ["b.child(c)", "c"]);
    }

    #[test]
    fn a_longer_name_is_another_function() {
        let s = Source::new("a.rs", "clock_text(1.0); my_clock(2.0); clock(3.0)");
        let args: Vec<&str> = s.calls("clock").into_iter().map(|(_, a)| a).collect();
        assert_eq!(args, ["3.0"]);
    }

    #[test]
    fn comments_are_left_out() {
        let s = Source::new("a.rs", "// clock(1.0)\nx(); // clock(2.0)\n");
        assert!(s.calls("clock").is_empty());
    }

    #[test]
    fn a_find_tells_its_line_in_the_file() {
        let s = Source::new("a.rs", "fn a() {\n    b\n        .c(1)\n}\n");
        let (at, _) = s.calls("c")[0];
        assert_eq!(s.line(at), 3);
    }
}
