//! Diffing a catalogue against a watch, and generating installers for browsers
//! that cannot write to a removable drive themselves.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::catalog::Target;
use crate::uapp::{AppId, Version};

/// One app as found on a connected watch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    /// Identity read from the header.
    pub app_id: AppId,
    /// Folder it lives in under `Apps\`.
    pub folder: String,
    /// The `.uapp` the watch would load.
    pub file: String,
    /// Display name from the header.
    pub name: String,
    /// Version from the header.
    pub version: Version,
    /// File length in bytes.
    pub size: usize,
    /// Any further `.uapp` files in the same folder.
    ///
    /// The watch loads whichever it finds first, so more than one means it may
    /// still be booting the older build.
    #[serde(default)]
    pub extra_uapps: Vec<String>,
    /// Hash of the code alone, once the file has been read in full.
    ///
    /// Absent after a header-only scan. Reading whole files off a USB volume is
    /// expensive, so callers deepen only what looks like an update.
    #[serde(default)]
    pub payload_sha256: Option<String>,
}

/// What should happen to an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Not on the watch at all.
    Install,
    /// A different build is on the watch.
    Update,
    /// The selected version is already installed.
    Current,
    /// The watch has something newer than the selected version, so installing
    /// would be a downgrade.
    NewerOnWatch,
}

/// A planned action for one app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// The version selected from the catalogue.
    pub app: Target,
    /// What should happen.
    pub status: Status,
    /// What is on the watch, when anything is.
    pub installed: Option<Installed>,
    /// Whether an update would change the version stamp but not the code.
    ///
    /// Only knowable once the installed file has been hashed.
    pub identical_payload: bool,
}

impl Entry {
    /// Whether this entry would write to the watch.
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        matches!(self.status, Status::Install | Status::Update)
    }

    /// Whether the installed file still needs hashing to classify this entry.
    #[must_use]
    pub fn needs_payload_hash(&self) -> bool {
        self.status == Status::Update
            && self
                .installed
                .as_ref()
                .is_some_and(|i| i.payload_sha256.is_none())
    }

    /// Human-readable reason this entry is in the plan.
    #[must_use]
    pub fn describe(&self) -> String {
        let Some(installed) = &self.installed else {
            return format!("install {}", self.app.version);
        };
        let move_ = format!("{} → {}", installed.version, self.app.version);
        if self.identical_payload {
            format!("{move_} · version stamp only, identical code")
        } else {
            move_
        }
    }
}

/// The result of comparing a catalogue selection against a watch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// One entry per catalogue app, in catalogue order.
    pub entries: Vec<Entry>,
    /// Apps on the watch that the catalogue does not know about. Reported, never
    /// touched.
    pub foreign: Vec<Installed>,
}

impl Plan {
    /// Entries that would be written.
    pub fn actionable(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_actionable())
    }

    /// Entries whose installed bytes still need hashing.
    pub fn needing_payload_hash(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.needs_payload_hash())
    }

    /// How many actions are pure version re-stamps.
    #[must_use]
    pub fn restamp_count(&self) -> usize {
        self.entries.iter().filter(|e| e.identical_payload).count()
    }
}

/// Compare selected versions against what is installed.
///
/// Keyed on [`AppId`], never on folder or display name: folders are arbitrary and
/// display names are not unique — upstream reassigned the ids of three Glances,
/// so two catalogue entries can share a name.
#[must_use]
pub fn build(targets: &[Target], installed: &[Installed]) -> Plan {
    let entries = targets
        .iter()
        .map(|target| {
            let on_watch = installed.iter().find(|i| i.app_id == target.app_id);

            let Some(on_watch) = on_watch else {
                return Entry {
                    app: target.clone(),
                    status: Status::Install,
                    installed: None,
                    identical_payload: false,
                };
            };

            let status = match target.version.cmp(&on_watch.version) {
                std::cmp::Ordering::Greater => Status::Update,
                std::cmp::Ordering::Less => Status::NewerOnWatch,
                // Same version but different bytes still warrants a rewrite: a
                // truncated or half-written install reports the right version.
                std::cmp::Ordering::Equal if on_watch.size != target.size => Status::Update,
                std::cmp::Ordering::Equal => Status::Current,
            };

            // Version moved but the code did not. Still offered, since installing
            // changes what the watch reports, but labelled rather than presented
            // as new work.
            let identical_payload = status == Status::Update
                && on_watch
                    .payload_sha256
                    .as_ref()
                    .is_some_and(|hash| *hash == target.payload_sha256);

            Entry {
                app: target.clone(),
                status,
                installed: Some(on_watch.clone()),
                identical_payload,
            }
        })
        .collect();

    let foreign = installed
        .iter()
        .filter(|i| !targets.iter().any(|t| t.app_id == i.app_id))
        .cloned()
        .collect();

    Plan { entries, foreign }
}

/// Where a generated installer should fetch binaries and find the watch.
#[derive(Debug, Clone)]
pub struct ScriptConfig {
    /// Base URL of the published `data/` directory.
    pub base_url: String,
    /// Windows volume label to match.
    pub windows_label: String,
    /// Unix mount point to default to.
    pub unix_mount: String,
}

impl Default for ScriptConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            windows_label: "UNA WATCH".into(),
            unix_mount: "/Volumes/UNA WATCH".into(),
        }
    }
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn job_note(entry: &Entry) -> String {
    let status = match entry.status {
        Status::Install => "install",
        Status::Update => "update",
        Status::Current => "current",
        Status::NewerOnWatch => "newer-on-watch",
    };
    if entry.identical_payload {
        format!("{status}; identical code, version stamp only")
    } else {
        status.to_owned()
    }
}

/// Generate the Windows installer.
///
/// Mirrors the ordering proven by `Update-Watch-Apps.ps1` in the UNA SDK: resolve
/// the drive by volume label, copy the new `.uapp` with `[IO.File]::Copy`
/// (scripted `Copy-Item` to this volume has produced silent corruption), verify,
/// and only then delete the stale binary. `settings.json` and `Activity\` are
/// never touched, so settings and recorded activities survive.
#[must_use]
pub fn powershell(plan: &Plan, config: &ScriptConfig) -> String {
    let mut out = String::new();
    let label = ps_quote(&config.windows_label);
    let base = ps_quote(&config.base_url);

    let _ = write!(
        out,
        "#Requires -Version 5.1\n\
         <#\n\
         \x20 Generated by Kira. Installs UNA Watch apps over USB mass storage.\n\
         \x20 Preserves each app's settings.json and Activity\\ data.\n\
         \x20 Unofficial; not affiliated with UNA Watch Ltd.\n\
         #>\n\
         Set-StrictMode -Version 2.0\n\
         $ErrorActionPreference = 'Stop'\n\
         \n\
         $Label   = {label}\n\
         $BaseUrl = {base}\n\
         \n\
         # Find the watch by volume label rather than a drive letter, which moves.\n\
         $vol = @(Get-Volume | Where-Object {{ $_.FileSystemLabel -eq $Label -and $_.DriveLetter }})\n\
         if ($vol.Count -ne 1) {{ throw \"Expected exactly one volume labelled '$Label', found $($vol.Count). Connect the watch.\" }}\n\
         $appsRoot = \"{{0}}:\\Apps\" -f $vol[0].DriveLetter\n\
         if (-not (Test-Path -LiteralPath $appsRoot)) {{ throw \"No Apps folder at $appsRoot.\" }}\n\
         Write-Host \"Watch apps folder: $appsRoot\"\n\
         \n\
         $tmp = Join-Path ([IO.Path]::GetTempPath()) (\"kira-\" + [Guid]::NewGuid().ToString(\"N\"))\n\
         [IO.Directory]::CreateDirectory($tmp) | Out-Null\n\
         try {{\n"
    );

    for entry in plan.actionable() {
        let app = &entry.app;
        let _ = write!(
            out,
            "\n  # {} {} ({})\n\
             \x20 $folder = {}; $file = {}; $sha = {}\n\
             \x20 $url = \"$BaseUrl/{}\"\n\
             \x20 $dl = Join-Path $tmp $file\n\
             \x20 Write-Host \"  downloading $folder/$file\"\n\
             \x20 Invoke-WebRequest -Uri $url -OutFile $dl -UseBasicParsing\n\
             \x20 $got = (Get-FileHash -LiteralPath $dl -Algorithm SHA256).Hash.ToLower()\n\
             \x20 if ($got -ne $sha) {{ throw \"$file : SHA-256 mismatch (expected $sha, got $got)\" }}\n\
             \x20 $dir = Join-Path $appsRoot $folder\n\
             \x20 [IO.Directory]::CreateDirectory($dir) | Out-Null\n\
             \x20 $dst = Join-Path $dir $file\n\
             \x20 # .NET copy, not Copy-Item: the latter has silently corrupted this volume.\n\
             \x20 [IO.File]::Copy($dl, $dst, $true)\n\
             \x20 if ((Get-Item -LiteralPath $dst).Length -ne (Get-Item -LiteralPath $dl).Length) {{\n\
             \x20   [IO.File]::Delete($dst); throw \"$file : size mismatch after copy; stale binary left in place\"\n\
             \x20 }}\n\
             \x20 # New binary is good, so it is now safe to drop any older .uapp. The watch\n\
             \x20 # loads the FIRST .uapp in the folder, so leaving two can boot the old one.\n\
             \x20 Get-ChildItem -LiteralPath $dir -Filter *.uapp -File |\n\
             \x20   Where-Object {{ $_.Name -ne $file }} |\n\
             \x20   ForEach-Object {{ [IO.File]::Delete($_.FullName); Write-Host \"    removed stale $($_.Name)\" }}\n\
             \x20 Write-Host \"  [ok] $folder -> $file\"\n",
            app.name,
            app.version,
            job_note(entry),
            ps_quote(&app.folder),
            ps_quote(&app.file),
            ps_quote(&app.sha256),
            app.download,
        );
    }

    out.push_str(
        "}\n\
         finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }\n\
         \n\
         Write-Host \"\"\n\
         Write-Host 'NEXT STEPS'\n\
         Write-Host '  1. Safely eject the watch (tray -> Safely Remove Hardware).'\n\
         Write-Host '  2. Reboot the watch: the launcher list is rebuilt only at boot.'\n\
         Write-Host '  3. Reconnect and re-run Kira''s Verify step to check the writes landed in flash.'\n",
    );
    out
}

/// Generate the macOS/Linux installer. Same ordering and guarantees.
#[must_use]
pub fn shell(plan: &Plan, config: &ScriptConfig) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "#!/bin/sh\n\
         # Generated by Kira. Installs UNA Watch apps over USB mass storage.\n\
         # Preserves each app's settings.json and Activity/ data.\n\
         # Unofficial; not affiliated with UNA Watch Ltd.\n\
         set -eu\n\
         \n\
         MOUNT=\"${{1:-{mount}}}\"\n\
         BASE_URL={base}\n\
         APPS=\"$MOUNT/Apps\"\n\
         [ -d \"$APPS\" ] || {{ echo \"No Apps folder at $APPS. Pass the mount point as the first argument.\" >&2; exit 1; }}\n\
         echo \"Watch apps folder: $APPS\"\n\
         \n\
         TMP=$(mktemp -d)\n\
         trap 'rm -rf \"$TMP\"' EXIT INT TERM\n\
         \n\
         sha256_of() {{\n\
         \x20 if command -v shasum >/dev/null 2>&1; then shasum -a 256 \"$1\" | cut -d\" \" -f1\n\
         \x20 else sha256sum \"$1\" | cut -d\" \" -f1; fi\n\
         }}\n",
        mount = config.unix_mount,
        base = sh_quote(&config.base_url),
    );

    for entry in plan.actionable() {
        let app = &entry.app;
        let _ = write!(
            out,
            "\n# {} {} ({})\n\
             folder={}; file={}; sha={}\n\
             echo \"  downloading $folder/$file\"\n\
             curl -fsSL \"$BASE_URL/{}\" -o \"$TMP/$file\"\n\
             got=$(sha256_of \"$TMP/$file\")\n\
             if [ \"$got\" != \"$sha\" ]; then echo \"$file: SHA-256 mismatch (expected $sha, got $got)\" >&2; exit 1; fi\n\
             mkdir -p \"$APPS/$folder\"\n\
             cp \"$TMP/$file\" \"$APPS/$folder/$file\"\n\
             # New binary is in place, so stale .uapp files can go. The watch loads the\n\
             # FIRST .uapp in a folder, so leaving two can boot the old one.\n\
             for old in \"$APPS/$folder\"/*.uapp; do\n\
             \x20 [ -e \"$old\" ] || continue\n\
             \x20 case \"$(basename \"$old\")\" in \"$file\") ;; *) rm -f \"$old\"; echo \"    removed stale $(basename \"$old\")\";; esac\n\
             done\n\
             echo \"  [ok] $folder -> $file\"\n",
            app.name,
            app.version,
            job_note(entry),
            sh_quote(&app.folder),
            sh_quote(&app.file),
            sh_quote(&app.sha256),
            app.download,
        );
    }

    out.push_str(
        "\nsync\n\
         echo\n\
         echo \"NEXT STEPS\"\n\
         echo \"  1. Eject the watch:  diskutil eject \\\"$MOUNT\\\"   (macOS)  /  udisksctl unmount  (Linux)\"\n\
         echo \"  2. Reboot the watch: the launcher list is rebuilt only at boot.\"\n\
         echo \"  3. Reconnect and re-run Kira's Verify step to check the writes landed in flash.\"\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uapp::AppType;

    fn target(version: &str) -> Target {
        let version: Version = version.parse().unwrap();
        Target {
            app_id: AppId::new(0xA19C_2A7E_4F8B_6D31),
            name: "Alarm".into(),
            app_type: AppType::Utility,
            icon: None,
            icon_small: None,
            folder: "Alarm".into(),
            file: format!("Alarm_{version}.uapp"),
            version,
            libc_version: Version::new(0, 0, 3),
            autostart: true,
            size: 210_628,
            sha256: "a".repeat(64),
            payload_sha256: "p".repeat(64),
            download: format!("apps/apps-v{version}/Alarm/Alarm_{version}.uapp"),
            tag: format!("apps-v{version}"),
            changed: Some(true),
            is_latest: true,
        }
    }

    fn installed(version: &str, size: usize) -> Installed {
        Installed {
            app_id: AppId::new(0xA19C_2A7E_4F8B_6D31),
            folder: "Alarm".into(),
            file: format!("Alarm_{version}.uapp"),
            name: "Alarm".into(),
            version: version.parse().unwrap(),
            size,
            extra_uapps: Vec::new(),
            payload_sha256: None,
        }
    }

    fn config() -> ScriptConfig {
        ScriptConfig {
            base_url: "https://example.test/data".into(),
            ..ScriptConfig::default()
        }
    }

    #[test]
    fn an_absent_app_is_an_install() {
        let plan = build(&[target("1.3.0")], &[]);
        assert_eq!(plan.entries[0].status, Status::Install);
        assert_eq!(plan.actionable().count(), 1);
    }

    #[test]
    fn an_older_version_on_the_watch_is_an_update() {
        let plan = build(&[target("1.3.0")], &[installed("1.2.0", 1000)]);
        assert_eq!(plan.entries[0].status, Status::Update);
    }

    #[test]
    fn the_same_version_and_size_is_current() {
        let plan = build(&[target("1.3.0")], &[installed("1.3.0", 210_628)]);
        assert_eq!(plan.entries[0].status, Status::Current);
        assert_eq!(plan.actionable().count(), 0);
    }

    #[test]
    fn the_same_version_at_a_different_size_is_an_update() {
        // A truncated install reports the correct version in its header, so
        // version alone cannot be the freshness test.
        let plan = build(&[target("1.3.0")], &[installed("1.3.0", 12345)]);
        assert_eq!(plan.entries[0].status, Status::Update);
    }

    #[test]
    fn a_newer_build_on_the_watch_is_not_downgraded() {
        let plan = build(&[target("1.2.0")], &[installed("1.3.0", 210_628)]);
        assert_eq!(plan.entries[0].status, Status::NewerOnWatch);
        assert_eq!(plan.actionable().count(), 0);
    }

    #[test]
    fn matching_is_by_app_id_not_folder() {
        let mut on_watch = installed("1.2.0", 1000);
        on_watch.folder = "MyAlarm".into();
        let plan = build(&[target("1.3.0")], &[on_watch]);
        assert_eq!(plan.entries[0].status, Status::Update);
        assert_eq!(
            plan.entries[0].installed.as_ref().unwrap().folder,
            "MyAlarm"
        );
    }

    #[test]
    fn a_different_id_in_a_same_named_folder_is_a_separate_app() {
        let mut on_watch = installed("1.2.0", 1000);
        on_watch.app_id = AppId::new(0x0123_4567_89AB_CDEF);
        let plan = build(&[target("1.3.0")], &[on_watch]);
        assert_eq!(plan.entries[0].status, Status::Install);
        assert_eq!(plan.foreign.len(), 1);
    }

    #[test]
    fn unknown_apps_are_reported_and_never_actioned() {
        let mut stranger = installed("1.0.0", 100);
        stranger.app_id = AppId::new(0xFFFF_FFFF_FFFF_FFFF);
        stranger.folder = "Squash".into();
        let plan = build(&[target("1.3.0")], &[installed("1.3.0", 210_628), stranger]);
        assert_eq!(plan.foreign.len(), 1);
        assert_eq!(plan.foreign[0].folder, "Squash");
        assert_eq!(plan.actionable().count(), 0);
    }

    #[test]
    fn a_restamp_is_labelled_but_still_offered() {
        let target = target("1.3.0");
        let mut on_watch = installed("1.2.0", 210_628);
        on_watch.payload_sha256 = Some(target.payload_sha256.clone());
        let plan = build(&[target], &[on_watch]);
        assert_eq!(plan.entries[0].status, Status::Update);
        assert!(plan.entries[0].identical_payload);
        assert_eq!(plan.actionable().count(), 1);
        assert!(plan.entries[0].describe().contains("identical code"));
        assert_eq!(plan.restamp_count(), 1);
    }

    #[test]
    fn without_a_hash_an_update_is_not_claimed_identical() {
        let plan = build(&[target("1.3.0")], &[installed("1.2.0", 210_628)]);
        assert!(!plan.entries[0].identical_payload);
        assert_eq!(plan.entries[0].describe(), "1.2.0 → 1.3.0");
        assert!(plan.entries[0].needs_payload_hash());
        assert_eq!(plan.needing_payload_hash().count(), 1);
    }

    #[test]
    fn the_powershell_script_writes_before_removing_the_stale_binary() {
        let plan = build(&[target("1.3.0")], &[]);
        let script = powershell(&plan, &config());
        let copy = script.find("[IO.File]::Copy").unwrap();
        let remove = script.find("removed stale").unwrap();
        assert!(remove > copy, "copy must precede stale removal");
        // Copy-Item has silently corrupted this volume; only invocations matter,
        // and the script explains the choice in a comment.
        assert!(
            !script
                .lines()
                .any(|l| l.trim_start().starts_with("Copy-Item"))
        );
        assert!(script.contains("FileSystemLabel -eq $Label"));
        assert!(script.contains("SHA256"));
    }

    #[test]
    fn the_shell_script_verifies_the_hash_before_copying() {
        let plan = build(&[target("1.3.0")], &[]);
        let script = shell(&plan, &config());
        let check = script.find("SHA-256 mismatch").unwrap();
        let copy = script.find("cp \"$TMP").unwrap();
        assert!(copy > check, "hash check must precede the copy");
        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("set -eu"));
    }

    #[test]
    fn neither_script_removes_an_app_folder() {
        let plan = build(&[target("1.3.0")], &[installed("1.2.0", 1000)]);
        for script in [powershell(&plan, &config()), shell(&plan, &config())] {
            assert!(!script.contains("rm -rf \"$APPS"));
            assert!(!script.contains("Remove-Item -Recurse -Force $dir"));
        }
    }

    #[test]
    fn an_empty_plan_still_produces_a_runnable_script() {
        let plan = build(&[target("1.3.0")], &[installed("1.3.0", 210_628)]);
        let script = shell(&plan, &config());
        assert!(script.starts_with("#!/bin/sh"));
        assert!(!script.contains("curl"));
    }

    #[test]
    fn a_display_name_holding_a_slash_never_reaches_a_path() {
        // GlanceARHR really is named "AVG / R HR". The name may appear in a
        // comment for readability, but every path must come from the folder.
        let mut app = target("1.3.0");
        app.name = "AVG / R HR".into();
        app.folder = "GlanceARHR".into();
        app.file = "AVG_R_HR_1.3.0.uapp".into();
        app.download = "apps/apps-v1.3.0/GlanceARHR/AVG_R_HR_1.3.0.uapp".into();
        let plan = build(&[app], &[]);

        for script in [powershell(&plan, &config()), shell(&plan, &config())] {
            for line in script.lines().filter(|l| l.contains("AVG / R HR")) {
                assert!(
                    line.trim_start().starts_with('#'),
                    "display name escaped into a non-comment line: {line}"
                );
            }
            assert!(script.contains("GlanceARHR"));
        }
    }

    #[test]
    fn quotes_in_a_folder_name_cannot_break_out() {
        let mut app = target("1.0.0");
        app.folder = "Bob's App".into();
        app.file = "Bob's_1.0.0.uapp".into();
        let plan = build(&[app], &[]);
        assert!(powershell(&plan, &config()).contains("'Bob''s App'"));
        assert!(shell(&plan, &config()).contains(r"'Bob'\''s App'"));
    }

    #[test]
    fn a_restamp_is_annotated_in_both_scripts() {
        let target = target("1.3.0");
        let mut on_watch = installed("1.2.0", 210_628);
        on_watch.payload_sha256 = Some(target.payload_sha256.clone());
        let plan = build(&[target], &[on_watch]);
        for script in [powershell(&plan, &config()), shell(&plan, &config())] {
            assert!(script.contains("identical code, version stamp only"));
        }
    }
}
