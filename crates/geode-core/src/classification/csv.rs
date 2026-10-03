//! A small RFC 4180 CSV reader and writer for classification import and
//! export. Fields may be quoted; a quoted field may hold commas, doubled
//! quotes and line breaks. CRLF and LF both end a record, a leading UTF-8 BOM
//! is ignored, and blank lines are skipped. `Record::line` is the physical
//! line the record starts on, for rejection messages.

/// One parsed record and the physical line it starts on (1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub line: usize,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for CsvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Close the current field and record. A blank line parses as one empty
/// field and is skipped, so it never becomes a one-field row to reject.
fn end_record(out: &mut Vec<Record>, fields: &mut Vec<String>, field: &mut String, start: usize) {
    fields.push(std::mem::take(field));
    let record = std::mem::take(fields);
    if !(record.len() == 1 && record[0].is_empty()) {
        out.push(Record {
            line: start,
            fields: record,
        });
    }
}

/// Parse `text`. Errors on an unterminated quoted field or a quote inside an
/// unquoted field: guessing would import a label the file did not say.
pub fn read(text: &str) -> Result<Vec<Record>, CsvError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false; // inside a quoted field
    let mut was_quoted = false; // the current field started with a quote
    let mut line = 1usize;
    let mut start = 1usize;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    field.push('\n');
                }
                '\r' if chars.peek() == Some(&'\n') => {}
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !was_quoted => {
                quoted = true;
                was_quoted = true;
            }
            '"' => {
                return Err(CsvError {
                    line,
                    message: "a quote inside an unquoted field".into(),
                });
            }
            ',' => {
                fields.push(std::mem::take(&mut field));
                was_quoted = false;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                end_record(&mut out, &mut fields, &mut field, start);
                was_quoted = false;
                line += 1;
                start = line;
            }
            c => {
                if was_quoted {
                    return Err(CsvError {
                        line,
                        message: "text after a closing quote".into(),
                    });
                }
                field.push(c);
            }
        }
    }
    if quoted {
        return Err(CsvError {
            line: start,
            message: "a quoted field never closes its quote".into(),
        });
    }
    if !field.is_empty() || !fields.is_empty() || was_quoted {
        end_record(&mut out, &mut fields, &mut field, start);
    }
    Ok(out)
}

fn needs_quotes(field: &str) -> bool {
    field.contains([',', '"', '\n', '\r']) || field.starts_with(' ') || field.ends_with(' ')
}

/// Render records, quoting a field only when it needs it; LF line endings,
/// no BOM, a final newline.
pub fn write(records: &[Vec<String>]) -> String {
    let mut out = String::new();
    for record in records {
        for (i, field) in record.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if needs_quotes(field) {
                out.push('"');
                out.push_str(&field.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(field);
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(records: &[Record]) -> Vec<Vec<&str>> {
        records
            .iter()
            .map(|r| r.fields.iter().map(String::as_str).collect())
            .collect()
    }

    #[test]
    fn reads_plain_and_quoted_fields() {
        let got = read("a,b\n\"x, y\",\"say \"\"hi\"\"\"\n").unwrap();
        assert_eq!(
            fields(&got),
            vec![vec!["a", "b"], vec!["x, y", "say \"hi\""]]
        );
    }

    #[test]
    fn excel_on_windows_bom_crlf_and_trailing_blank_line() {
        let got =
            read("\u{feff}underlying_ref,sector\r\nAAPL,\"Consumer, Cyclical\"\r\n\r\n").unwrap();
        assert_eq!(
            fields(&got),
            vec![
                vec!["underlying_ref", "sector"],
                vec!["AAPL", "Consumer, Cyclical"]
            ]
        );
    }

    #[test]
    fn a_quoted_line_break_stays_in_the_field_and_lines_count_physically() {
        let got = read("h1,h2\n\"two\nlines\",x\nlast,y\n").unwrap();
        assert_eq!(got[1].fields[0], "two\nlines");
        assert_eq!(got[1].line, 2);
        assert_eq!(got[2].line, 4);
    }

    #[test]
    fn blank_lines_between_records_are_skipped_and_empty_fields_kept() {
        let got = read("a,b\n\nc,\n").unwrap();
        assert_eq!(fields(&got), vec![vec!["a", "b"], vec!["c", ""]]);
        assert_eq!(got[1].line, 3);
    }

    #[test]
    fn an_unterminated_quote_is_an_error_naming_its_line() {
        let err = read("a,b\n\"open,x\n").unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.message.contains("quote"), "{}", err.message);
    }

    #[test]
    fn a_quote_inside_an_unquoted_field_is_an_error() {
        let err = read("a,b\nab\"c,d\n").unwrap_err();
        assert_eq!(err.line, 2);
    }

    #[test]
    fn write_quotes_only_when_needed_and_round_trips() {
        let records = vec![
            vec!["underlying_ref".to_string(), "sector".to_string()],
            vec!["AAPL".to_string(), "Consumer, Cyclical".to_string()],
            vec!["Q\"T".to_string(), " padded ".to_string()],
            vec!["NL".to_string(), "a\nb".to_string()],
        ];
        let text = write(&records);
        assert!(
            text.starts_with("underlying_ref,sector\nAAPL,\"Consumer, Cyclical\"\n"),
            "{text}"
        );
        let back: Vec<Vec<String>> = read(&text).unwrap().into_iter().map(|r| r.fields).collect();
        assert_eq!(back, records);
    }
}
