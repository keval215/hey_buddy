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

/// The only two ACL commands the hooks may run, compared after trimming the line.
const GRANT_DIR: &str =
    r#"nsExec::ExecToLog '"$SYSDIR\icacls.exe" "$INSTDIR" /grant "*S-1-15-2-1:(RX)"'"#;
const GRANT_AGENT: &str = r#"nsExec::ExecToLog '"$SYSDIR\icacls.exe" "$INSTDIR\buddy-agent.exe" /grant "*S-1-15-2-1:(RX)"'"#;

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// True when `token` occurs in `text` (ASCII case-insensitive) starting at a word boundary.
/// Whitespace, tabs, `/`, `:` and so on all count as boundaries.
fn has_token(text: &str, token: &str) -> bool {
    let (t, k) = (text.to_ascii_lowercase(), token.to_ascii_lowercase());
    t.match_indices(&k)
        .any(|(i, _)| !t[..i].chars().next_back().is_some_and(is_word))
}

/// Body of `!macro NAME ... !macroend` (comments already removed), if present.
fn macro_body<'a>(code: &'a str, name: &str) -> Option<&'a str> {
    let start = code.find(&format!("!macro {name}"))?;
    let rest = &code[start..];
    let end = rest.find("!macroend")?;
    Some(&rest[..end])
}

/// The NSIS hooks may only grant ALL APPLICATION PACKAGES `(RX)` on the install dir (not
/// inherited) and on buddy-agent.exe, must guard against drive roots / user folders, and must
/// delete nothing. Structural: exact lines, not a substring blacklist.
pub fn check_hooks(h: &str) -> Vec<String> {
    let lines: Vec<&str> = h
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
        .collect();
    let code = lines.join("\n");
    let mut bad = Vec::new();
    for want in [GRANT_DIR, GRANT_AGENT] {
        let n = lines.iter().filter(|l| **l == want).count();
        if n != 1 {
            bad.push(format!(
                "hooks: expected exactly one line `{want}`, found {n}"
            ));
        }
    }
    for l in &lines {
        let low = l.to_ascii_lowercase();
        let acl_cmd = ["icacls", "cacls", "takeown", "/grant", "/deny", "/setowner"]
            .iter()
            .any(|k| low.contains(k));
        if acl_cmd && *l != GRANT_DIR && *l != GRANT_AGENT {
            bad.push(format!("hooks: unexpected ACL command line: {l}"));
        }
        if l.contains("S-1-15-2-1") && *l != GRANT_DIR && *l != GRANT_AGENT {
            bad.push(format!("hooks: unexpected use of S-1-15-2-1: {l}"));
        }
    }
    for forbidden in [
        "/grant:r",
        "/inheritance",
        "/reset",
        "/remove",
        "Everyone",
        "S-1-1-0",
        "RequestExecutionLevel admin",
        "runas",
    ] {
        if code
            .to_ascii_lowercase()
            .contains(&forbidden.to_ascii_lowercase())
        {
            bad.push(format!("hooks: forbidden token {forbidden}"));
        }
    }
    // Fail-closed guard for user-selectable install dirs.
    for var in [
        "$PROFILE",
        "$DOCUMENTS",
        "$DESKTOP",
        "$LOCALAPPDATA",
        "$PROGRAMFILES",
    ] {
        if !code.contains(var) {
            bad.push(format!("hooks: install-dir guard does not mention {var}"));
        }
    }
    if !code.contains("MessageBox") {
        bad.push("hooks: guard must warn with a MessageBox when it skips the grant".into());
    }
    // Nothing may delete anything (the uninstall macros especially; S3-6 owns data deletion).
    for name in ["NSIS_HOOK_PREUNINSTALL", "NSIS_HOOK_POSTUNINSTALL"] {
        match macro_body(&code, name) {
            None => bad.push(format!("hooks: macro {name} missing")),
            Some(body) => {
                for t in ["Delete", "RMDir", "RemoveDirectory"] {
                    if has_token(body, t) {
                        bad.push(format!("hooks: {name} contains forbidden token {t}"));
                    }
                }
            }
        }
    }
    for t in ["RMDir", "RemoveDirectory", "Delete"] {
        if has_token(&code, t) {
            bad.push(format!("hooks: forbidden token {t}"));
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
    /// The real hooks file: the mutation tests below start from what actually ships.
    const HOOKS: &str = include_str!("../../../apps/desktop/src-tauri/installer/hooks.nsh");

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
    fn shipped_hooks_pass() {
        assert_eq!(check_hooks(HOOKS), Vec::<String>::new());
    }
    fn mutated(from: &str, to: &str) -> Vec<String> {
        assert!(HOOKS.contains(from), "test setup: {from}");
        check_hooks(&HOOKS.replace(from, to))
    }
    #[test]
    fn hooks_negatives() {
        // Comments never count.
        assert!(!check_hooks("; only a comment S-1-15-2-1 (RX) icacls").is_empty());
        // Wrong SID, wider perms, inheritance.
        assert!(!mutated("*S-1-15-2-1:(RX)", "*S-1-1-0:(RX)").is_empty());
        assert!(!mutated("*S-1-15-2-1:(RX)", "*S-1-15-2-2:(RX)").is_empty());
        assert!(!mutated("*S-1-15-2-1:(RX)", "*S-1-15-2-1:(RX)(WD)").is_empty());
        assert!(!mutated("*S-1-15-2-1:(RX)", "*S-1-15-2-1:(RX,DE)").is_empty());
        assert!(!mutated("*S-1-15-2-1:(RX)", "*S-1-15-2-1:(OI)(CI)(RX)").is_empty());
        assert!(!mutated("*S-1-15-2-1:(RX)", "*S-1-15-2-1:(F)").is_empty());
        assert!(!mutated(
            "/grant \"*S-1-15-2-1:(RX)\"'\n    Pop",
            "/grant:r \"*S-1-15-2-1:(RX)\"'\n    Pop"
        )
        .is_empty());
        // Extra ACL commands.
        for extra in [
            "nsExec::Exec 'icacls \"$PROFILE\" /inheritance:r'",
            "nsExec::Exec 'icacls x /reset'",
            "nsExec::Exec 'takeown /f x'",
            "nsExec::Exec 'cacls x /G Everyone:F'",
            "ExecWait 'cmd /c icacls x /grant *S-1-15-2-1:(RX)'",
        ] {
            assert!(
                !mutated(
                    "!macro NSIS_HOOK_PREINSTALL",
                    &format!("!macro NSIS_HOOK_PREINSTALL\n  {extra}")
                )
                .is_empty(),
                "{extra}"
            );
        }
        // A duplicated or missing grant line.
        assert!(!mutated(
            "!macro NSIS_HOOK_PREINSTALL",
            &format!("!macro NSIS_HOOK_PREINSTALL\n{GRANT_DIR}")
        )
        .is_empty());
        assert!(!mutated(GRANT_AGENT, "").is_empty());
    }
    #[test]
    fn hooks_delete_tokens_in_uninstall_fail() {
        for t in [
            "Delete\t$INSTDIR\\x",
            "Delete /REBOOTOK x",
            "delete x",
            "RMDir /r $APPDATA",
            "rmdir\t$APPDATA",
            "RemoveDirectory x",
            "DeleteRegKey HKCU x",
        ] {
            let m = mutated(
                "!macro NSIS_HOOK_POSTUNINSTALL",
                &format!("!macro NSIS_HOOK_POSTUNINSTALL\n  {t}"),
            );
            assert!(!m.is_empty(), "{t}");
            let m = mutated(
                "!macro NSIS_HOOK_PREUNINSTALL",
                &format!("!macro NSIS_HOOK_PREUNINSTALL\n{t}"),
            );
            assert!(!m.is_empty(), "{t}");
        }
        // A word that merely contains the token is fine.
        assert!(mutated(
            "!macro NSIS_HOOK_POSTUNINSTALL",
            "!macro NSIS_HOOK_POSTUNINSTALL\n  DetailPrint \"Undeleted\""
        )
        .is_empty());
    }
    #[test]
    fn hooks_guard_must_exist() {
        assert!(!mutated("$PROFILE", "$TEMP").is_empty());
        assert!(!mutated("MessageBox", "DetailPrint").is_empty());
        assert!(!check_hooks("").is_empty());
    }
}
