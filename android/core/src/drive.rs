use crate::download::allowed_url;
use anyhow::{Context, Result, ensure};
use scraper::{Html, Selector};
use url::Url;

#[derive(Debug, PartialEq)]
pub struct FileRef {
    pub id: String,
    pub resource_key: Option<String>,
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
impl FileRef {
    pub fn parse(input: &str) -> Result<Self> {
        if valid_id(input) {
            return Ok(Self {
                id: input.into(),
                resource_key: None,
            });
        }
        let url = Url::parse(input).context("expected a Drive file ID or sharing URL")?;
        ensure!(
            allowed_url(&url),
            "expected a Google Drive HTTPS sharing URL"
        );
        let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
        let id = parts
            .windows(2)
            .find(|p| p[0] == "d")
            .map(|p| p[1].to_owned())
            .or_else(|| {
                url.query_pairs()
                    .find(|(k, _)| k == "id")
                    .map(|(_, v)| v.into_owned())
            })
            .context("no file ID in URL (folders are not supported)")?;
        ensure!(valid_id(&id), "invalid Drive file ID");
        let resource_key = url
            .query_pairs()
            .find(|(k, _)| k == "resourcekey")
            .map(|(_, v)| v.into_owned());
        if let Some(key) = &resource_key {
            ensure!(valid_id(key), "invalid resource key");
        }
        Ok(Self { id, resource_key })
    }
    pub fn url(&self, authenticated: bool, export: Option<&str>) -> Result<Url> {
        ensure!(
            export.is_none() || authenticated,
            "--export requires gdirect login or GDIRECT_ACCESS_TOKEN"
        );
        let mut url = if authenticated {
            Url::parse(&format!(
                "https://www.googleapis.com/drive/v3/files/{}{}",
                self.id,
                if export.is_some() { "/export" } else { "" }
            ))?
        } else {
            Url::parse("https://drive.usercontent.google.com/download")?
        };
        {
            let mut q = url.query_pairs_mut();
            if authenticated {
                if let Some(mime) = export {
                    q.append_pair("mimeType", mime);
                } else {
                    q.append_pair("alt", "media")
                        .append_pair("supportsAllDrives", "true");
                }
            } else {
                q.append_pair("id", &self.id)
                    .append_pair("export", "download")
                    .append_pair("confirm", "t");
            }
            if let Some(key) = &self.resource_key {
                q.append_pair("resourcekey", key);
            }
        }
        Ok(url)
    }
}

pub fn confirmation(html: &str, base: &Url) -> Result<Url> {
    let document = Html::parse_document(html);
    let form_selector = Selector::parse("form#download-form").unwrap();
    let input_selector = Selector::parse("input[name]").unwrap();
    let form = document.select(&form_selector).next().context("Drive returned an HTML page instead of a file; check sharing permissions, quota or authentication")?;
    ensure!(
        form.value()
            .attr("method")
            .unwrap_or("get")
            .eq_ignore_ascii_case("get"),
        "unsupported confirmation method"
    );
    let mut target = base.join(
        form.value()
            .attr("action")
            .context("confirmation form has no action")?,
    )?;
    ensure!(
        allowed_url(&target),
        "untrusted confirmation form destination"
    );
    for input in form.select(&input_selector) {
        target.query_pairs_mut().append_pair(
            input.value().attr("name").unwrap(),
            input.value().attr("value").unwrap_or(""),
        );
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links() {
        assert_eq!(
            FileRef::parse("https://drive.google.com/file/d/abc_123/view?resourcekey=key-1")
                .unwrap(),
            FileRef {
                id: "abc_123".into(),
                resource_key: Some("key-1".into())
            }
        );
        assert_eq!(
            FileRef::parse("https://drive.google.com/open?id=abc")
                .unwrap()
                .id,
            "abc"
        );
        assert!(FileRef::parse("https://evil.test/file/d/abc").is_err());
        assert!(FileRef::parse("https://drive.google.com/drive/folders/abc").is_err());
        assert!(FileRef::parse("a/b").is_err());
    }
    #[test]
    fn confirm_form() {
        let base = Url::parse("https://drive.usercontent.google.com/download").unwrap();
        let u = confirmation(r#"<form id="download-form" action="/download"><input name="uuid" value="a&amp;b"><input name="confirm" value="t"></form>"#, &base).unwrap();
        assert!(u.query_pairs().any(|(k, v)| k == "uuid" && v == "a&b"));
        assert!(
            confirmation(
                r#"<form id="download-form" action="https://evil.test/"></form>"#,
                &base
            )
            .is_err()
        );
        assert!(confirmation("<html>quota exceeded</html>", &base).is_err());
    }
}
