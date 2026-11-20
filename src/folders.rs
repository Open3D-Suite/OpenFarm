// SPDX-License-Identifier: GPL-3.0-or-later
//! Where OpenFarm keeps its files: `config.json` in `%APPDATA%\Open3D\OpenFarm`, and everything else
//! (its log, the window's web view, the farm server's `server` folder, the agent's `agent` folder) in
//! `%LOCALAPPDATA%\Open3D\OpenFarm`, or in `OpenFarm` inside the folders that `OPEN3D_SETTINGS_DIR` and
//! `OPEN3D_DATA_DIR` name (D-042, D-044). `--data <folder>` keeps everything in that one folder, as it
//! always did, and nothing is copied for it.
//!
//! Before that, OpenFarm kept everything in `%LOCALAPPDATA%\OpenFarm` (`%TEMP%\OpenFarm` without
//! `LOCALAPPDATA`). The first start with the new folders copies from it, once, and leaves it as it was
//! (D-048, D-049): its `config.json`, which holds the machine's identity and its key, and its `server`
//! folder, the farm (the database with its machines, approvals and jobs, the scenes, the previews and
//! the logs of the tasks). The rest is made again: the log starts anew, the web view keeps nothing the
//! page needs, the agent writes its host scripts at every start, downloads again the scenes it needs
//! and checks them, and keeps render outputs only while a task runs.
//!
//! [`Plan`] says where the files will be without touching anything, so that `main` can first make
//! sure no other OpenFarm runs (one holds the port of the local app); [`Plan::prepare`] then copies.
//! When a copy fails, that run uses the old folder for what was not copied, as before, and the next
//! start tries again.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::SETTINGS_FILE;
use crate::open3d_settings::{self, CarryOver};

/// The app's name: its Open3D folders, and its old folder in `%LOCALAPPDATA%`.
pub const APP: &str = "OpenFarm";
/// Only debug builds read it: a folder in place of `%LOCALAPPDATA%\OpenFarm`, the old folder, so that
/// a test never reads the person's own.
pub const TEST_OLD_FOLDER: &str = "OPENFARM_TEST_OLD_FOLDER";
/// The farm server's folder, inside the data folder.
const SERVER: &str = "server";

/// Where this run will keep its files, before anything is copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// `--data`: everything in that folder, and nothing copied.
    Given(PathBuf),
    /// The Open3D folders, and the old folder they are carried over from. A missing Open3D folder
    /// (neither its variable nor `APPDATA` or `LOCALAPPDATA` is set) is the old folder, as before.
    Open3D { old: PathBuf, settings: Option<PathBuf>, data: Option<PathBuf> },
}

/// The folders this run uses, once the old ones have been carried over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folders {
    /// `config.json`.
    pub settings: PathBuf,
    /// The log, the web view and the agent's folder.
    pub data: PathBuf,
    /// The folder whose `server` holds the farm: `data`, or the old folder in a run whose copy of the
    /// farm failed.
    pub farm: PathBuf,
    /// What could not be carried over, to tell the person.
    pub problems: Vec<String>,
}

impl Plan {
    /// The folders of this computer: those `--data` names, otherwise the Open3D folders. It reads
    /// only the environment.
    pub fn new(given: Option<PathBuf>) -> Plan {
        match given {
            Some(dir) => Plan::Given(dir),
            None => Plan::Open3D { old: old_folder(), settings: open3d_settings::app_settings_dir(APP), data: open3d_settings::app_data_dir(APP) },
        }
    }

    /// The folder whose `config.json` this run will read, as things are before anything is copied:
    /// the new settings folder once it is there, until then the old folder it is copied from.
    pub fn settings_now(&self) -> &Path {
        match self {
            Plan::Given(dir) => dir,
            Plan::Open3D { settings: Some(new), .. } if exists(new) => new,
            Plan::Open3D { old, .. } => old,
        }
    }

    /// Carries the old folder over into the new ones, once, and gives the folders of this run. Call
    /// it before anything reads or writes in them: the log, the settings, the farm, the web view.
    pub fn prepare(self) -> Folders {
        let (old, settings, data) = match self {
            Plan::Given(dir) => return Folders { settings: dir.clone(), data: dir.clone(), farm: dir, problems: Vec::new() },
            Plan::Open3D { old, settings, data } => (old, settings, data),
        };
        let mut problems = Vec::new();
        let settings = match settings {
            Some(new) => {
                let outcome = carry_settings(&old, &new);
                use_or_keep(old.clone(), new, outcome, &old, &mut problems)
            }
            None => old.clone(),
        };
        let data = data.unwrap_or_else(|| old.clone());
        let farm = if data == old {
            old.clone()
        } else {
            let outcome = carry_farm(&old.join(SERVER), &data.join(SERVER));
            use_or_keep(old.clone(), data.clone(), outcome, &old.join(SERVER), &mut problems)
        };
        Folders { settings, data, farm, problems }
    }
}

/// The folder to use after carrying something over: `new`, or `old` when the copy failed, so that
/// this run does not make the new copy and lose it. What went wrong goes into `problems`; `copied`
/// is the old folder the paths of the files left out are in.
fn use_or_keep(old: PathBuf, new: PathBuf, outcome: CarryOver, copied: &Path, problems: &mut Vec<String>) -> PathBuf {
    match outcome {
        CarryOver::Nothing => new,
        CarryOver::Copied { left_out, .. } => {
            if !left_out.is_empty() {
                let files = left_out.iter().map(|(path, why)| format!("{} ({why})", copied.join(path).display())).collect::<Vec<_>>();
                problems.push(format!("not copied: {}", files.join(", ")));
            }
            new
        }
        CarryOver::Failed(reason) => {
            problems.push(format!("{reason}; until the copy is made, OpenFarm keeps using {} as before", old.display()));
            old
        }
    }
}

/// The folder OpenFarm used before the Open3D folders: `%LOCALAPPDATA%\OpenFarm`, or
/// `%TEMP%\OpenFarm` without `LOCALAPPDATA`.
fn old_folder() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os(TEST_OLD_FOLDER).filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("LOCALAPPDATA").filter(|dir| !dir.is_empty()).map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(APP)
}

/// Whether something has the name `path`: a folder, a file or a link. When that cannot be told, it
/// counts as there, so that it is never touched.
fn exists(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(e) => !matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory),
    }
}

/// Copies the farm, `server` in the old folder, with [`open3d_settings::carry_over`]. Its database
/// is held while it is copied, so that no program writes it half way; when a program has it open to
/// write, an OpenFarm that is still running on it, nothing is copied.
fn carry_farm(old: &Path, new: &Path) -> CarryOver {
    if exists(new) || !old.is_dir() {
        return CarryOver::Nothing;
    }
    let _held = match hold_database(old) {
        Ok(held) => held,
        Err(reason) => return CarryOver::Failed(reason),
    };
    match open3d_settings::carry_over(old, new) {
        CarryOver::Failed(reason) => CarryOver::Failed(format!("couldn't copy the farm: {reason}")),
        outcome => outcome,
    }
}

/// Opens the farm's database in `server` so that, while it stays open, programs may read it but
/// none may open it to write. Fails when one already has it open to write. `None` without a database.
fn hold_database(server: &Path) -> Result<Option<std::fs::File>, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    let path = server.join(crate::server::db::FILE);
    match std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("couldn't copy the farm: {} is in use by another program, perhaps another OpenFarm ({e})", path.display())),
    }
}

/// Copies `config.json` from the old folder into the new settings folder `new`. The old folder
/// holds the farm too, and [`open3d_settings::carry_over`] copies whole folders, so this copies the
/// one file by the same rules: only when nothing has the name `new` and the old folder has the file;
/// into a folder of its own beside `new`, which takes the name `new` once the copy is complete; with
/// the time the file was last changed; and the old file is left as it was.
fn carry_settings(old: &Path, new: &Path) -> CarryOver {
    let file = old.join(SETTINGS_FILE);
    if exists(new) || !file.is_file() {
        return CarryOver::Nothing;
    }
    let (Some(parent), Some(name)) = (new.parent(), new.file_name()) else {
        return CarryOver::Failed(format!("{} cannot be a folder of its own", new.display()));
    };
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_nanos());
    let mut temporary = name.to_os_string();
    temporary.push(format!(".carry-{}-{stamp}.tmp", std::process::id()));
    let temporary = parent.join(temporary);
    if let Err(e) = std::fs::create_dir_all(parent).and_then(|()| std::fs::create_dir(&temporary)) {
        return CarryOver::Failed(format!("couldn't copy the settings: couldn't create a folder in {} ({e})", parent.display()));
    }
    match copy_file(&file, &temporary.join(SETTINGS_FILE)).and_then(|()| put_in_place(&temporary, new)) {
        Ok(()) => CarryOver::Copied { files: 1, left_out: Vec::new() },
        Err(reason) => {
            let _ = std::fs::remove_dir_all(&temporary);
            // Another start may have put its own copy in place meanwhile.
            if exists(new) { CarryOver::Nothing } else { CarryOver::Failed(format!("couldn't copy the settings: {reason}")) }
        }
    }
}

/// Copies the file `from` into the new file `to`, with the time it was last changed, and makes sure
/// the copy is on the disk.
fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to).map_err(|e| format!("couldn't read {} ({e})", from.display()))?;
    std::fs::OpenOptions::new().write(true).open(to).and_then(|file| file.sync_all()).map_err(|e| format!("couldn't write {} ({e})", to.display()))
}

/// Gives the finished copy its name, `new`. While nothing else has that name it tries a few times:
/// another program, such as an antivirus, may hold the copy for a moment.
fn put_in_place(temporary: &Path, new: &Path) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        if exists(new) {
            return Err(format!("{} appeared while the copy was made", new.display()));
        }
        match std::fs::rename(temporary, new) {
            Ok(()) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("couldn't rename {} to {} ({last})", temporary.display(), new.display()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::TestDir;
    use crate::config::Config;
    use crate::server::db::{Db, StoredMachine};
    use crate::server::farm::Farm;

    /// The folders of a test, all inside its own folder: `Local\OpenFarm` stands for the old one,
    /// `Roaming\Open3D\OpenFarm` and `Local\Open3D\OpenFarm` for the new ones.
    struct Places {
        dir: TestDir,
    }

    impl Places {
        fn new() -> Places {
            Places { dir: TestDir::new() }
        }
        fn old(&self) -> PathBuf {
            self.dir.path().join("Local").join(APP)
        }
        fn settings(&self) -> PathBuf {
            self.dir.path().join("Roaming").join("Open3D").join(APP)
        }
        fn data(&self) -> PathBuf {
            self.dir.path().join("Local").join("Open3D").join(APP)
        }
        fn plan(&self) -> Plan {
            Plan::Open3D { old: self.old(), settings: Some(self.settings()), data: Some(self.data()) }
        }
        /// The folders when everything is copied, or there was nothing to copy.
        fn new_folders(&self) -> Folders {
            Folders { settings: self.settings(), data: self.data(), farm: self.data(), problems: Vec::new() }
        }
    }

    /// Makes the files `entries` (path with "/", contents) inside `dir`.
    fn make(dir: &Path, entries: &[(&str, &str)]) {
        for (path, text) in entries {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
    }

    /// Every file and folder inside `dir`, by its path with "/": a file with its bytes, a folder with
    /// none. Empty when `dir` is missing.
    fn snapshot(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        fn walk(path: &Path, key: &str, found: &mut BTreeMap<String, Option<Vec<u8>>>) {
            let Ok(entries) = std::fs::read_dir(path) else { return };
            for entry in entries {
                let entry = entry.unwrap();
                let name = format!("{key}{}", entry.file_name().to_string_lossy());
                if entry.file_type().unwrap().is_dir() {
                    found.insert(name.clone(), None);
                    walk(&entry.path(), &format!("{name}/"), found);
                } else {
                    found.insert(name, Some(std::fs::read(entry.path()).unwrap()));
                }
            }
        }
        let mut found = BTreeMap::new();
        walk(dir, "", &mut found);
        found
    }

    /// The names in a folder, in order; none when it is missing.
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map_or_else(|_| Vec::new(), |entries| entries.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect());
        names.sort();
        names
    }

    /// What OpenFarm 0.1 leaves in its folder after some work.
    const OLD: &[(&str, &str)] = &[
        ("config.json", r#"{"role":"agent","machine_id":"m-1","local_port":47302}"#),
        ("logs/openfarm.log", "started"),
        ("webview/EBWebView/Local State", "{}"),
        ("server/farm.sqlite", "the farm"),
        ("server/scenes/3f2a/shot.max", "a scene"),
        ("server/thumbs/1/7.jpg", "a preview"),
        ("server/logs/12.log", "a task"),
        ("agent/hosts/max_host.py", "a host"),
        ("agent/cache/scenes/3f2a/shot.max", "a scene"),
        ("agent/logs/max-cpu.log", "a host's log"),
    ];

    #[test]
    fn the_first_start_copies_the_settings_and_the_farm_once_and_leaves_the_old_folder() {
        let places = Places::new();
        make(&places.old(), OLD);
        let before = snapshot(&places.old());
        // Before the copy, the port of the local app comes from the old settings.
        assert_eq!(places.plan().settings_now(), places.old());
        assert_eq!(places.plan().prepare(), places.new_folders());
        let copied = snapshot(&places.settings());
        assert_eq!(copied.keys().collect::<Vec<_>>(), ["config.json"]);
        assert_eq!(copied["config.json"], before["config.json"]);
        assert_eq!(snapshot(&places.data().join(SERVER)), snapshot(&places.old().join(SERVER)));
        // The log, the web view and the agent's folder are made again.
        assert_eq!(names_in(&places.data()), [SERVER]);
        assert_eq!(snapshot(&places.old()), before, "the old folder is left as it was");
        // Nothing else beside the new folders.
        assert_eq!(names_in(&places.dir.path().join("Roaming").join("Open3D")), [APP]);
        assert_eq!(names_in(&places.dir.path().join("Local").join("Open3D")), [APP]);
        assert_eq!(names_in(&places.dir.path().join("Local")), ["Open3D", APP]);

        // The next start finds the new folders and copies nothing: what they hold is used as it is.
        assert_eq!(places.plan().settings_now(), places.settings());
        std::fs::write(places.settings().join(SETTINGS_FILE), "{}").unwrap();
        std::fs::remove_file(places.data().join("server/thumbs/1/7.jpg")).unwrap();
        let (settings, data) = (snapshot(&places.settings()), snapshot(&places.data()));
        assert_eq!(places.plan().prepare(), places.new_folders());
        assert_eq!((snapshot(&places.settings()), snapshot(&places.data())), (settings, data));
        assert_eq!(snapshot(&places.old()), before);
    }

    #[test]
    fn the_machine_key_opens_from_the_new_folder() {
        let places = Places::new();
        // A machine set up by an earlier OpenFarm: its key is protected by Windows for this user.
        let earlier = Config::load_or_create(&places.old()).unwrap();
        let text = std::fs::read(places.old().join(SETTINGS_FILE)).unwrap();
        let folders = places.plan().prepare();
        let moved = Config::load_or_create(&folders.settings).unwrap();
        assert_eq!(folders.settings, places.settings());
        assert_eq!(moved.identity().unwrap().fingerprint(), earlier.identity().unwrap().fingerprint());
        assert_eq!(moved.machine_id, earlier.machine_id);
        // The key was read, not made again, so the settings were not written again either.
        assert_eq!(std::fs::read(places.settings().join(SETTINGS_FILE)).unwrap(), text);
        assert_eq!(std::fs::read(places.old().join(SETTINGS_FILE)).unwrap(), text);
    }

    /// The farm in `dir`, the folder that holds `server`, as the farm server opens it.
    fn open_farm(dir: &Path) -> Farm {
        Farm::open("Studio farm".into(), dir.join(SERVER), tokio::sync::watch::channel(1).0).unwrap()
    }

    /// A farm made in `dir` (the folder that holds `server`) that knows one approved machine.
    fn a_farm_with_a_machine(dir: &Path) {
        drop(open_farm(dir));
        Db::open(&dir.join(SERVER).join(crate::server::db::FILE))
            .unwrap()
            .save_machine(&StoredMachine {
                id: "node-07".into(),
                name: "NODE-07".into(),
                fingerprint: Some("f1".into()),
                approved: true,
                enabled: true,
                caps: Default::default(),
                rank: 0,
                last_seen: 0,
            })
            .unwrap();
    }

    #[test]
    fn the_farm_opens_from_the_new_folder_with_its_machines() {
        let places = Places::new();
        a_farm_with_a_machine(&places.old());
        let folders = places.plan().prepare();
        assert_eq!(folders, places.new_folders());
        let farm = open_farm(&folders.farm);
        assert!(farm.machines["node-07"].approved);
        assert!(farm.trusts_fingerprint("f1"));
    }

    #[test]
    fn a_farm_another_program_has_open_is_not_copied_and_is_used_where_it_is() {
        let places = Places::new();
        a_farm_with_a_machine(&places.old());
        make(&places.old(), &[("config.json", "{}")]);
        // Another OpenFarm, an earlier one or one on another port, still has the database open.
        let other = Db::open(&places.old().join(SERVER).join(crate::server::db::FILE)).unwrap();
        let before = snapshot(&places.old());
        let folders = places.plan().prepare();
        assert_eq!((folders.settings.clone(), folders.data.clone()), (places.settings(), places.data()), "the settings are copied");
        assert_eq!(folders.farm, places.old());
        assert_eq!(folders.problems.len(), 1, "{:?}", folders.problems);
        assert!(
            folders.problems[0].contains("in use") && folders.problems[0].contains(&places.old().display().to_string()),
            "{}",
            folders.problems[0]
        );
        assert!(!places.data().exists());
        assert_eq!(snapshot(&places.old()), before);
        // Once it is closed, the next start copies the farm.
        drop(other);
        assert_eq!(places.plan().prepare(), places.new_folders());
        assert!(places.data().join(SERVER).join(crate::server::db::FILE).is_file());
    }

    #[test]
    fn a_copy_that_fails_leaves_the_old_folder_in_use_and_says_why() {
        let places = Places::new();
        make(&places.old(), OLD);
        let before = snapshot(&places.old());
        // The folders the new ones go in are files.
        make(places.dir.path(), &[("Roaming/Open3D", "a file"), ("Local/Open3D", "a file")]);
        let folders = places.plan().prepare();
        assert_eq!((folders.settings, folders.farm), (places.old(), places.old()));
        assert_eq!(folders.problems.len(), 2, "{:?}", folders.problems);
        assert!(folders.problems[0].starts_with("couldn't copy the settings"), "{}", folders.problems[0]);
        assert!(folders.problems[1].starts_with("couldn't copy the farm"), "{}", folders.problems[1]);
        assert_eq!(snapshot(&places.old()), before);
        assert_eq!(names_in(&places.dir.path().join("Roaming")), ["Open3D"]);
        assert_eq!(names_in(&places.dir.path().join("Local")), ["Open3D", APP]);
    }

    #[test]
    fn the_data_option_keeps_everything_in_its_folder_and_copies_nothing() {
        let places = Places::new();
        make(&places.old(), OLD);
        let given = places.dir.path().join("test farm");
        let plan = Plan::new(Some(given.clone()));
        assert_eq!(plan.settings_now(), given);
        let folders = plan.prepare();
        assert_eq!(folders, Folders { settings: given.clone(), data: given.clone(), farm: given.clone(), problems: Vec::new() });
        assert!(!given.exists() && !places.settings().exists() && !places.data().exists());
    }

    #[test]
    fn without_the_open3d_folders_the_old_one_is_used_as_before() {
        let places = Places::new();
        make(&places.old(), OLD);
        let old = places.old();
        let neither = Plan::Open3D { old: old.clone(), settings: None, data: None };
        assert_eq!(neither.settings_now(), old);
        assert_eq!(neither.prepare(), Folders { settings: old.clone(), data: old.clone(), farm: old.clone(), problems: Vec::new() });
        // With only the settings folder, only the settings are copied.
        let settings_only = Plan::Open3D { old: old.clone(), settings: Some(places.settings()), data: None };
        assert_eq!(settings_only.prepare(), Folders { settings: places.settings(), data: old.clone(), farm: old, problems: Vec::new() });
        assert_eq!(names_in(&places.settings()), [SETTINGS_FILE]);
        assert!(!places.data().exists());
    }

    #[test]
    fn without_an_old_folder_nothing_is_copied_or_made() {
        let places = Places::new();
        assert_eq!(places.plan().prepare(), places.new_folders());
        assert!(!places.settings().exists() && !places.data().exists());
        // An old folder without settings or farm: nothing to copy either.
        make(&places.old(), &[("logs/openfarm.log", "started")]);
        assert_eq!(places.plan().prepare(), places.new_folders());
        assert!(!places.settings().exists() && !places.data().exists());
    }

    #[test]
    fn the_copied_settings_keep_the_time_they_were_changed() {
        let places = Places::new();
        make(&places.old(), &[("config.json", "{}")]);
        let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        std::fs::File::options().write(true).open(places.old().join(SETTINGS_FILE)).unwrap().set_modified(then).unwrap();
        places.plan().prepare();
        assert_eq!(std::fs::metadata(places.settings().join(SETTINGS_FILE)).unwrap().modified().unwrap(), then);
    }
}
