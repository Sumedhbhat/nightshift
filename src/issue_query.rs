//! The supported GitHub-style subset for filtering fetched open issues.
use crate::github::Issue;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IssueQuery {
    terms: Vec<Term>,
}

#[derive(Clone, Debug, PartialEq)]
struct Term {
    exclude: bool,
    field: Field,
}

#[derive(Clone, Debug, PartialEq)]
enum Field {
    Title(String),
    Label(Vec<String>),
    Repository(String),
    NoLabel,
}

impl IssueQuery {
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut tokens = Vec::new();
        let mut token = String::new();
        let mut quoted = false;
        for ch in input.chars() {
            match ch {
                '"' => quoted = !quoted,
                ch if ch.is_whitespace() && !quoted => {
                    if !token.is_empty() {
                        tokens.push(std::mem::take(&mut token));
                    }
                }
                ch => token.push(ch),
            }
        }
        if quoted {
            return Err("Close the quotation mark to apply this search.".into());
        }
        if !token.is_empty() {
            tokens.push(token);
        }
        let mut terms = Vec::new();
        for token in tokens {
            if matches!(token.as_str(), "OR" | "AND" | "NOT") || token.contains(['(', ')']) {
                return Err("Boolean expressions are not supported. Separate filters with spaces to match all of them.".into());
            }
            let exclude = token.starts_with('-');
            let token = token.strip_prefix('-').unwrap_or(&token);
            if token.is_empty() {
                return Err("Add a title or qualifier after '-'.".into());
            }
            let field = if let Some((key, value)) = token.split_once(':') {
                if value.is_empty() {
                    return Err(format!("Add a value after {key}:"));
                }
                let value = value.to_lowercase();
                match key.to_lowercase().as_str() {
                    "label" => {
                        let labels: Vec<_> = value.split(',').map(str::to_owned).collect();
                        if labels.iter().any(String::is_empty) {
                            return Err(
                                "Give each label a value, or remove the extra comma.".into()
                            );
                        }
                        Field::Label(labels)
                    }
                    "repo" => Field::Repository(value),
                    "no" if value == "label" => Field::NoLabel,
                    "is" if matches!(value.as_str(), "open" | "issue") && !exclude => continue,
                    "state" if value == "open" && !exclude => continue,
                    "in" if value == "title" && !exclude => continue,
                    "is" | "state" => {
                        return Err(
                            "This view searches open issues only. Use is:open is:issue.".into()
                        );
                    }
                    _ => {
                        return Err(format!(
                            "Unsupported filter '{key}:'. Use label:, -label:, repo:, no:label, or title text."
                        ));
                    }
                }
            } else {
                Field::Title(token.to_lowercase())
            };
            terms.push(Term { exclude, field });
        }
        Ok(Self { terms })
    }

    pub fn matches(&self, repository: &str, issue: &Issue) -> bool {
        self.terms.iter().all(|term| {
            let matches = match &term.field {
                Field::Title(text) => issue.title.to_lowercase().contains(text),
                Field::Label(labels) => labels.iter().any(|wanted| {
                    issue
                        .labels
                        .iter()
                        .any(|label| label.to_lowercase() == *wanted)
                }),
                Field::Repository(repo) => repository.eq_ignore_ascii_case(repo),
                Field::NoLabel => issue.labels.is_empty(),
            };
            matches != term.exclude
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn issue() -> Issue {
        Issue {
            number: 42,
            title: "Fix keyboard search café".into(),
            labels: vec!["ready-for-agent".into(), "help wanted".into()],
            url: String::new(),
        }
    }
    #[test]
    fn combines_title_labels_and_repository() {
        let query = IssueQuery::parse("is:open is:issue in:title KEYBOARD label:ready-for-agent label:\"help wanted\" repo:Owner/Repo").unwrap();
        assert!(query.matches("owner/repo", &issue()));
        assert!(!query.matches("another/repo", &issue()));
        assert!(
            !IssueQuery::parse("label:bug")
                .unwrap()
                .matches("owner/repo", &issue())
        );
    }
    #[test]
    fn supports_exclusion_label_alternatives_and_phrases() {
        assert!(
            IssueQuery::parse("\"keyboard search\" -label:bug label:bug,ready-for-agent café")
                .unwrap()
                .matches("owner/repo", &issue())
        );
        assert!(
            !IssueQuery::parse("-label:\"help wanted\"")
                .unwrap()
                .matches("owner/repo", &issue())
        );
        assert!(
            !IssueQuery::parse("-keyboard")
                .unwrap()
                .matches("owner/repo", &issue())
        );
        assert!(
            !IssueQuery::parse("no:label")
                .unwrap()
                .matches("owner/repo", &issue())
        );
        assert!(
            IssueQuery::parse("")
                .unwrap()
                .matches("owner/repo", &issue())
        );
    }
    #[test]
    fn rejects_incomplete_and_unsupported_queries() {
        for query in [
            "label:",
            "label:\"help",
            "assignee:me",
            "is:closed",
            "state:closed",
            "is:pr",
            "-is:open",
            "in:body",
            "label:a OR label:b",
            "label:bug,",
            "-",
        ] {
            assert!(IssueQuery::parse(query).is_err(), "{query}");
        }
    }
    #[test]
    fn repeated_labels_require_both_but_commas_accept_either() {
        assert!(
            !IssueQuery::parse("label:bug label:ready-for-agent")
                .unwrap()
                .matches("owner/repo", &issue())
        );
        assert!(
            IssueQuery::parse("label:bug,ready-for-agent")
                .unwrap()
                .matches("owner/repo", &issue())
        );
        let unlabeled = Issue {
            labels: vec![],
            ..issue()
        };
        assert!(
            IssueQuery::parse("no:label")
                .unwrap()
                .matches("owner/repo", &unlabeled)
        );
    }
}
