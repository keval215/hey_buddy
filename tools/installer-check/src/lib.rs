//! Pure checks for the Windows installer (S3-1). No network, no platform code: everything takes
//! text or bytes so it runs on Linux CI.

use serde_json::Value;

/// Strip XML comments (a comment that merely mentions `uiAccess="true"` must not count).
pub fn strip_xml_comments(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => return out, // unterminated comment: drop the tail
        }
    }
    out.push_str(rest);
    out
}

/// Value of attribute `name` in a tag's text, accepting single or double quotes.
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = tag[from..].find(name) {
        let at = from + i;
        let before_ok = tag[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        let after = tag[at + name.len()..].trim_start();
        if before_ok && after.starts_with('=') {
            let v = after[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let end = v[1..].find(q)?;
                return Some(v[1..1 + end].to_string());
            }
        }
        from = at + name.len();
    }
    None
}

/// All `<requestedExecutionLevel .../>` tags (comments stripped).
fn exec_level_tags(xml: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<requestedExecutionLevel") {
        let after = &rest[i..];
        let end = after.find('>').unwrap_or(after.len());
        tags.push(after[..end].to_string());
        rest = &after[end..];
    }
    tags
}

/// Check a Windows application manifest. Returns the failures (empty = pass).
pub fn check_manifest(xml: &str) -> Vec<String> {
    let xml = strip_xml_comments(xml);
    let mut bad = Vec::new();
    if !xml.contains("<assembly") {
        bad.push("manifest: not an assembly manifest".into());
    }
    if !xml.contains("PerMonitorV2") {
        bad.push("manifest: dpiAwareness PerMonitorV2 missing".into());
    }
    let tags = exec_level_tags(&xml);
    if tags.is_empty() {
        bad.push("manifest: requestedExecutionLevel missing (must be asInvoker)".into());
    }
    for t in &tags {
        match attr(t, "level").as_deref() {
            Some("asInvoker") => {}
            Some(other) => bad.push(format!("manifest: requestedExecutionLevel level={other}")),
            None => bad.push("manifest: requestedExecutionLevel has no level".into()),
        }
        if let Some(v) = attr(t, "uiAccess") {
            if !v.eq_ignore_ascii_case("false") {
                bad.push(format!("manifest: uiAccess={v} (must be false)"));
            }
        }
    }
    if xml.contains("requireAdministrator") || xml.contains("highestAvailable") {
        bad.push("manifest: requires administrator / highestAvailable".into());
    }
    bad
}

/// Find the first embedded UTF-8 `<assembly ...>...</assembly>` manifest in a PE file's bytes.
pub fn extract_manifest(bytes: &[u8]) -> Option<String> {
    fn find(h: &[u8], n: &[u8], from: usize) -> Option<usize> {
        h.get(from..)?
            .windows(n.len())
            .position(|w| w == n)
            .map(|p| p + from)
    }
    let start = find(bytes, b"<assembly", 0)?;
    let end_tag = b"</assembly>";
    let end = find(bytes, end_tag, start)? + end_tag.len();
    String::from_utf8(bytes[start..end].to_vec()).ok()
}

/// Static checks of `tauri.conf.json` merged with the installer overlay. `hooks` is the text of
/// the NSIS hooks file (`None` = could not be read).
pub fn check_config(base: &Value, overlay: &Value, hooks: Option<&str>) -> Vec<String> {
    let mut bad = Vec::new();
    let bundle = &base["bundle"];
    if base["identifier"] != "dev.heybuddy.desktop" {
        bad.push("config: identifier must be dev.heybuddy.desktop".into());
    }
    if base["productName"].as_str().is_none_or(str::is_empty) {
        bad.push("config: productName missing".into());
    }
    if bundle["active"] != true {
        bad.push("config: bundle.active must be true".into());
    }
    if bundle["targets"] != serde_json::json!(["nsis"]) {
        bad.push(
            "config: bundle.targets must be exactly [\"nsis\"] (MSI only behind a flag)".into(),
        );
    }
    let win = &bundle["windows"];
    let nsis = &win["nsis"];
    if nsis["installMode"] != "currentUser" {
        bad.push("config: nsis.installMode must be currentUser (per-user, no admin)".into());
    }
    if nsis["displayLanguageSelector"] != false {
        bad.push("config: nsis.displayLanguageSelector must be false".into());
    }
    if nsis["installerHooks"].as_str().is_none_or(str::is_empty) {
        bad.push("config: nsis.installerHooks missing".into());
    }
    let wv = &win["webviewInstallMode"];
    if wv["type"] != "downloadBootstrapper" || wv["silent"] != false {
        bad.push(
            "config: webviewInstallMode must be downloadBootstrapper with silent=false".into(),
        );
    }
    // Signing is out of scope: no signing material may be configured.
    for k in [
        "certificateThumbprint",
        "signCommand",
        "timestampUrl",
        "tsp",
    ] {
        if !win[k].is_null() {
            bad.push(format!(
                "config: bundle.windows.{k} is set (installer must be unsigned in S3-1)"
            ));
        }
    }
    // The agent must be a resource that lands next to the main exe.
    let agent_ok = overlay["bundle"]["resources"]
        .as_object()
        .is_some_and(|m| m.values().any(|v| v == "buddy-agent.exe"));
    if !agent_ok {
        bad.push(
            "overlay: bundle.resources must map buddy-agent.exe to the install dir root".into(),
        );
    }
    match hooks {
        None => bad.push("hooks: installer hooks file unreadable".into()),
        Some(h) => bad.extend(check_hooks(h)),
    }
    bad
}

/// The NSIS hooks must grant ALL APPLICATION PACKAGES read+execute only, and delete nothing.
pub fn check_hooks(h: &str) -> Vec<String> {
    let code: String = h
        .lines()
        .filter(|l| !l.trim_start().starts_with(';'))
        .collect::<Vec<_>>()
        .join("\n");
    let mut bad = Vec::new();
    if !code.contains("S-1-15-2-1") || !code.contains("(RX)") || !code.contains("icacls") {
        bad.push("hooks: no icacls (RX) grant to ALL APPLICATION PACKAGES (S-1-15-2-1)".into());
    }
    for forbidden in [
        "(F)", "(M)", "(W)", "(WD)", "(GA)", "(GW)", "Everyone", "S-1-1-0",
    ] {
        if code.contains(forbidden) {
            bad.push(format!("hooks: forbidden ACL token {forbidden}"));
        }
    }
    for forbidden in ["RMDir", "Delete ", "RequestExecutionLevel admin", "runas"] {
        if code.contains(forbidden) {
            bad.push(format!("hooks: forbidden token {forbidden}"));
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const GOOD: &str = r#"<assembly><application><windowsSettings>
        <dpiAwareness>PerMonitorV2</dpiAwareness></windowsSettings></application>
        <trustInfo><security><requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
        </requestedPrivileges></security></trustInfo></assembly>"#;

    #[test]
    fn good_manifest_passes() {
        assert_eq!(check_manifest(GOOD), Vec::<String>::new());
    }
    #[test]
    fn missing_dpi_fails() {
        let m = GOOD.replace("PerMonitorV2", "system");
        assert!(check_manifest(&m)
            .iter()
            .any(|f| f.contains("PerMonitorV2")));
    }
    #[test]
    fn ui_access_true_fails_in_both_quote_styles() {
        for m in [
            GOOD.replace(r#"uiAccess="false""#, r#"uiAccess="true""#),
            GOOD.replace(r#"uiAccess="false""#, "uiAccess='true'"),
        ] {
            assert!(
                check_manifest(&m).iter().any(|f| f.contains("uiAccess")),
                "{m}"
            );
        }
    }
    #[test]
    fn require_administrator_fails() {
        let m = GOOD.replace("asInvoker", "requireAdministrator");
        let bad = check_manifest(&m);
        assert!(bad.iter().any(|f| f.contains("requireAdministrator")));
        let m = GOOD.replace("asInvoker", "highestAvailable");
        assert!(!check_manifest(&m).is_empty());
    }
    #[test]
    fn missing_level_fails() {
        let m = GOOD.replace(
            r#"<requestedExecutionLevel level="asInvoker" uiAccess="false" />"#,
            "",
        );
        assert!(check_manifest(&m).iter().any(|f| f.contains("missing")));
    }
    #[test]
    fn comments_are_ignored_both_ways() {
        // A bad word inside a comment does not fail ...
        let m = format!("<!-- requireAdministrator uiAccess=\"true\" -->{GOOD}");
        assert!(check_manifest(&m).is_empty());
        // ... and a good word inside a comment does not rescue a bad manifest.
        let m = GOOD.replace("PerMonitorV2", "system") + "<!-- PerMonitorV2 -->";
        assert!(!check_manifest(&m).is_empty());
    }
    #[test]
    fn extracts_manifest_from_binary_noise() {
        let mut b = b"MZ\x00\x01garbage".to_vec();
        b.extend_from_slice(GOOD.as_bytes());
        b.extend_from_slice(b"\x00tail");
        assert_eq!(extract_manifest(&b).as_deref(), Some(GOOD));
        assert!(extract_manifest(b"MZ nothing here").is_none());
        assert!(extract_manifest(b"<assembly unterminated").is_none());
    }

    fn good_base() -> Value {
        json!({"identifier":"dev.heybuddy.desktop","productName":"hey_buddy","bundle":{
            "active":true,"targets":["nsis"],"windows":{
            "webviewInstallMode":{"type":"downloadBootstrapper","silent":false},
            "nsis":{"installMode":"currentUser","displayLanguageSelector":false,
                    "installerHooks":"installer/hooks.nsh"}}}})
    }
    fn good_overlay() -> Value {
        json!({"bundle":{"resources":{"../../../target/release/buddy-agent.exe":"buddy-agent.exe"}}})
    }
    const HOOKS: &str = "; comment (F)\nnsExec::ExecToLog 'icacls \"$INSTDIR\" /grant \"*S-1-15-2-1:(OI)(CI)(RX)\"'\n";

    #[test]
    fn good_config_passes() {
        assert_eq!(
            check_config(&good_base(), &good_overlay(), Some(HOOKS)),
            Vec::<String>::new()
        );
    }
    #[test]
    fn config_negatives() {
        let mutate = |f: &dyn Fn(&mut Value)| {
            let mut b = good_base();
            f(&mut b);
            check_config(&b, &good_overlay(), Some(HOOKS))
        };
        assert!(
            !mutate(&|b| b["bundle"]["windows"]["nsis"]["installMode"] = json!("perMachine"))
                .is_empty()
        );
        assert!(
            !mutate(&|b| b["bundle"]["windows"]["nsis"]["installMode"] = json!("both")).is_empty()
        );
        assert!(!mutate(&|b| b["bundle"]["targets"] = json!("all")).is_empty());
        assert!(!mutate(&|b| b["bundle"]["targets"] = json!(["nsis", "msi"])).is_empty());
        assert!(
            !mutate(&|b| b["bundle"]["windows"]["webviewInstallMode"]["silent"] = json!(true))
                .is_empty()
        );
        assert!(
            !mutate(&|b| b["bundle"]["windows"]["certificateThumbprint"] = json!("AB")).is_empty()
        );
        assert!(!mutate(&|b| b["bundle"]["windows"]["signCommand"] = json!("x")).is_empty());
        assert!(!mutate(&|b| b["identifier"] = json!("x.y")).is_empty());
        assert!(!check_config(&good_base(), &json!({}), Some(HOOKS)).is_empty());
        assert!(!check_config(&good_base(), &good_overlay(), None).is_empty());
    }
    #[test]
    fn hooks_negatives() {
        assert!(check_hooks(HOOKS).is_empty());
        assert!(!check_hooks("; only a comment S-1-15-2-1 (RX) icacls").is_empty());
        assert!(!check_hooks("icacls x /grant *S-1-15-2-1:(F) (RX)").is_empty());
        assert!(!check_hooks(&format!("{HOOKS}\nRMDir /r $APPDATA")).is_empty());
    }
}
