//! In-app hook setup: everything scripts/install.sh does for the hook side,
//! embedded in the app so `brew install` / plain downloads need no companion
//! script. Also reachable headless via the `--setup-hooks` CLI flag.

use serde_json::{json, Map, Value};
use std::fs;
use std::path::PathBuf;

// The single source of truth for the hook script is the repo root notify.sh;
// it is embedded at build time so the app can (re)install it anywhere.
const NOTIFY_SH: &str = include_str!("../../../notify.sh");
const CLAUDE_ICON: &[u8] = include_bytes!("../assets/claude.png");
const COPILOT_ICON: &[u8] = include_bytes!("../assets/copilot.png");

const CLAUDE_EVENTS: &[(&str, &str)] = &[
    ("Stop", "stop"),
    ("Notification", "waiting"),
    ("PostToolUseFailure", "failure"),
    ("TaskCompleted", "task-done"),
    ("PreCompact", "compact"),
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "working"),
    ("SessionEnd", "session-end"),
];

const COPILOT_EVENTS: &[(&str, &str)] = &[
    ("sessionStart", "session-start"),
    ("sessionEnd", "session-end"),
    ("userPromptSubmitted", "working"),
    ("agentStop", "stop"),
    ("postToolUseFailure", "failure"),
    ("preCompact", "compact"),
    ("notification", "waiting"),
];

fn home() -> Result<PathBuf, String> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| "HOME is not set".to_string())
}

/// True when ~/.claude/settings.json already routes hooks through notify.sh
/// (either our installed copy or a repo checkout).
pub fn hooks_installed() -> bool {
    let Ok(home) = home() else { return false };
    let Ok(text) = fs::read_to_string(home.join(".claude/settings.json")) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    v.get("hooks")
        .map(|h| h.to_string().contains("notify.sh"))
        .unwrap_or(false)
}

fn claude_hook_entry(cmd: String, matcher: Option<&str>) -> Value {
    let mut entry = Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".into(), json!(m));
    }
    entry.insert(
        "hooks".into(),
        json!([{ "type": "command", "command": cmd, "async": true, "timeout": 10 }]),
    );
    json!([Value::Object(entry)])
}

pub fn setup_hooks_impl() -> Result<String, String> {
    let home = home()?;
    let claude_dir = home.join(".claude");
    fs::create_dir_all(&claude_dir).map_err(|e| e.to_string())?;

    // notify.sh
    let sh_path = claude_dir.join("notify.sh");
    fs::write(&sh_path, NOTIFY_SH).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sh_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    // banner icons (embedded — no network needed)
    let icons = claude_dir.join("notify-icons");
    fs::create_dir_all(&icons).map_err(|e| e.to_string())?;
    fs::write(icons.join("claude.png"), CLAUDE_ICON).map_err(|e| e.to_string())?;
    fs::write(icons.join("copilot.png"), COPILOT_ICON).map_err(|e| e.to_string())?;

    // Claude Code hooks (merge into settings.json, backup first)
    let settings_path = claude_dir.join("settings.json");
    let mut root: Value = match fs::read_to_string(&settings_path) {
        Ok(text) => {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = claude_dir.join(format!("settings.json.backup.{ts}"));
            fs::copy(&settings_path, &backup).map_err(|e| e.to_string())?;
            serde_json::from_str(&text)
                .map_err(|e| format!("settings.json is not valid JSON: {e}"))?
        }
        Err(_) => json!({}),
    };
    if !root.is_object() {
        return Err("settings.json is not a JSON object".into());
    }

    let sh = sh_path.to_string_lossy();
    let obj = root.as_object_mut().unwrap();
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("settings.json \"hooks\" is not an object")?;
    for (event, arg) in CLAUDE_EVENTS {
        hooks.insert(
            (*event).into(),
            claude_hook_entry(format!("{sh} {arg}"), None),
        );
    }
    hooks.insert(
        "PreToolUse".into(),
        claude_hook_entry(format!("{sh} question"), Some("AskUserQuestion")),
    );
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    // Copilot CLI hooks
    let copilot_dir = home.join(".copilot/hooks");
    fs::create_dir_all(&copilot_dir).map_err(|e| e.to_string())?;
    let mut copilot_hooks = Map::new();
    for (event, arg) in COPILOT_EVENTS {
        copilot_hooks.insert(
            (*event).into(),
            json!([{ "type": "command", "bash": format!("{sh} {arg} copilot"), "timeoutSec": 10 }]),
        );
    }
    fs::write(
        copilot_dir.join("notify.json"),
        serde_json::to_string_pretty(&json!({ "version": 1, "hooks": copilot_hooks }))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    Ok(format!(
        "Hooks installed. Script: {} · Claude Code: {} events · Copilot CLI: {} events. \
         Restart running agent sessions to activate.",
        sh,
        CLAUDE_EVENTS.len() + 1,
        COPILOT_EVENTS.len()
    ))
}
