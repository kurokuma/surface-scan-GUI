use crate::model::Record;
use anyhow::{Context, Result};
use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

fn needs_csv_escape(text: &str) -> bool {
    text.trim_start().starts_with(['=', '+', '-', '@']) || text.starts_with(['\t', '\r', '\n'])
}

pub fn load(path: &Path) -> Result<Vec<std::sync::Arc<Record>>> {
    use std::io::Read;
    const MAX_BYTES: u64 = 512 * 1024 * 1024;
    let file = fs::File::open(path)?;
    anyhow::ensure!(
        file.metadata()?.len() <= MAX_BYTES,
        "結果ファイルは512 MiB以内にしてください"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "結果ファイルは512 MiB以内にしてください"
    );
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let rows: Vec<Record> = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        serde_json::from_slice(bytes).context("結果JSONはレコード配列で指定してください")?
    } else {
        let mut reader = csv::Reader::from_reader(bytes);
        let headers = reader.headers()?.clone();
        let unique: std::collections::HashSet<_> = headers.iter().collect();
        anyhow::ensure!(unique.len() == headers.len(), "CSVに重複した列名があります");
        for required in ["ip", "port", "scheme"] {
            anyhow::ensure!(
                headers.iter().any(|s| s == required),
                "CSVに{required}列がありません"
            );
        }
        let schema = serde_json::to_value(Record::default())?;
        let mut rows = Vec::new();
        for (line, row) in reader.records().enumerate() {
            anyhow::ensure!(line < 100_000, "読み込める結果は10万件までです");
            let row = row.with_context(|| format!("CSV {}行目", line + 2))?;
            let escaped: Option<Vec<String>> = headers
                .iter()
                .position(|s| s == "__csv_escaped_fields")
                .map(|i| serde_json::from_str(row.get(i).unwrap_or("[]")))
                .transpose()?;
            let mut object = serde_json::Map::new();
            for (key, cell) in headers.iter().zip(row.iter()) {
                let Some(default) = schema.get(key) else {
                    continue;
                };
                let value = if default.is_string() {
                    let escaped_here = escaped
                        .as_ref()
                        .map(|keys| keys.iter().any(|k| k == key))
                        .unwrap_or_else(|| cell.strip_prefix('\'').is_some_and(needs_csv_escape));
                    serde_json::Value::String(
                        if escaped_here {
                            cell.strip_prefix('\'').unwrap_or(cell)
                        } else {
                            cell
                        }
                        .to_owned(),
                    )
                } else if cell.is_empty() {
                    default.clone()
                } else {
                    let cell = if escaped.is_none() {
                        cell.strip_prefix('\'')
                            .filter(|s| needs_csv_escape(s))
                            .unwrap_or(cell)
                    } else {
                        cell
                    };
                    serde_json::from_str(cell)
                        .with_context(|| format!("CSV {}行目の{key}が不正です", line + 2))?
                };
                object.insert(key.into(), value);
            }
            rows.push(
                serde_json::from_value(serde_json::Value::Object(object))
                    .with_context(|| format!("CSV {}行目の型が不正です", line + 2))?,
            );
        }
        rows
    };
    anyhow::ensure!(rows.len() <= 100_000, "読み込める結果は10万件までです");
    for (i, row) in rows.iter().enumerate() {
        row.browser_url()
            .with_context(|| format!("結果 {} 件目の接続先が不正です", i + 1))?;
    }
    Ok(rows.into_iter().map(std::sync::Arc::new).collect())
}

pub fn save<T: serde::Serialize>(path: &Path, rows: &[T], json: bool) -> Result<()> {
    // Write alongside the destination first; never leave half a JSON/CSV in the chosen file.
    let name = path
        .file_name()
        .context("ファイル名がありません")?
        .to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let result = (|| -> Result<()> {
        let mut output = BufWriter::new(file);
        if json {
            serde_json::to_writer_pretty(&mut output, rows)?;
        } else {
            output.write_all(b"\xef\xbb\xbf")?; // UTF-8 BOM for Excel on Windows.
            let mut writer = csv::Writer::from_writer(&mut output);
            // Derive CSV columns from the same schema as JSON, preserving every field.
            let schema = serde_json::to_value(Record::default())?;
            let keys: Vec<_> = schema.as_object().unwrap().keys().cloned().collect();
            writer.write_record(
                keys.iter()
                    .map(String::as_str)
                    .chain(std::iter::once("__csv_escaped_fields")),
            )?;
            for row in rows {
                let value = serde_json::to_value(row)?;
                let mut escaped = Vec::new();
                let mut cells: Vec<String> = keys
                    .iter()
                    .map(|key| {
                        let v = &value[key];
                        let text = match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Null => String::new(),
                            _ => v.to_string(),
                        };
                        // Spreadsheet formula injection protection; JSON preserves original strings.
                        if v.is_string() && needs_csv_escape(&text) {
                            escaped.push(key.clone());
                            format!("'{text}")
                        } else {
                            text
                        }
                    })
                    .collect();
                cells.push(serde_json::to_string(&escaped)?);
                writer.write_record(cells)?;
            }
            writer.flush()?;
        }
        output.flush()?;
        output.get_ref().sync_all()?;
        drop(output);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_csv_formula_protection() {
        let base = std::env::temp_dir().join(format!("fingerprint-test-{}", std::process::id()));
        let json = base.with_extension("json");
        let csv = base.with_extension("csv");
        let record = Record {
            title: "=1+2".into(),
            body: "日本語\n\"quoted\"".into(),
            ..Default::default()
        };
        save(&json, std::slice::from_ref(&record), true).unwrap();
        let rows: Vec<Record> = serde_json::from_slice(&fs::read(&json).unwrap()).unwrap();
        assert_eq!(rows[0].body, record.body);
        save(&csv, &[record], false).unwrap();
        let text = fs::read_to_string(&csv).unwrap();
        assert!(text.contains("'=1+2"));
        assert!(text.contains("response_headers"));
        assert!(text.contains("certificate") || text.contains("tls"));
        fs::remove_file(json).unwrap();
        fs::remove_file(csv).unwrap();
    }

    #[test]
    fn existing_temporary_file_is_never_deleted() {
        let path =
            std::env::temp_dir().join(format!("fingerprint-collision-{}.json", std::process::id()));
        let tmp = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        fs::write(&tmp, "existing data").unwrap();
        assert!(save(&path, &[Record::default()], true).is_err());
        assert_eq!(fs::read_to_string(&tmp).unwrap(), "existing data");
        fs::remove_file(tmp).unwrap();
    }

    #[test]
    fn imports_current_and_legacy_exports_without_losing_strings_or_tls() {
        let path =
            std::env::temp_dir().join(format!("fingerprint-import-{}.csv", std::process::id()));
        let mut row = Record {
            ip: "127.0.0.1".into(),
            port: 443,
            scheme: "https".into(),
            title: "=中文标题".into(),
            body: "'=literal\n繁體中文,\"quoted\"".into(),
            favicon_mmh3: Some(-123456),
            ..Default::default()
        };
        row.cookies = vec!["a=1".into(), "b=2".into()];
        row.tls = Some(crate::model::TlsInfo {
            subject: "中文证书".into(),
            ..Default::default()
        });
        save(&path, std::slice::from_ref(&row), false).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&loaded[0]).unwrap(),
            serde_json::to_value(&row).unwrap()
        );
        let json = path.with_extension("json");
        save(&json, std::slice::from_ref(&row), true).unwrap();
        assert_eq!(
            serde_json::to_value(&load(&json).unwrap()[0]).unwrap(),
            serde_json::to_value(&row).unwrap()
        );
        fs::write(&path,"\u{feff}ip,port,scheme,title,favicon_mmh3,body\r\n127.0.0.1,80,http,'=中文,'-123,test\r\n").unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded[0].title, "=中文");
        assert_eq!(loaded[0].favicon_mmh3, Some(-123));
        fs::write(
            &json,
            r#"[{"ip":"127.0.0.1","port":80,"scheme":"http","title":"中文"}]"#,
        )
        .unwrap();
        assert_eq!(load(&json).unwrap()[0].title, "中文");
        fs::write(&json, r#"[{"ip":"bad","port":80,"scheme":"file"}]"#).unwrap();
        assert!(load(&json).is_err());
        let domain_error = Record {
            target: "example.com".into(),
            port: 443,
            scheme: "https".into(),
            error: "DNS failed".into(),
            ..Default::default()
        };
        for (file, is_json) in [(&json, true), (&path, false)] {
            save(file, std::slice::from_ref(&domain_error), is_json).unwrap();
            let restored = load(file).unwrap();
            assert_eq!(restored[0].target, "example.com");
            assert!(restored[0].ip.is_empty());
            assert_eq!(restored[0].error, "DNS failed");
        }
        fs::remove_file(path).unwrap();
        fs::remove_file(json).unwrap();
    }
}
