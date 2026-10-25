use super::*;
use cimon_recipe::tree::MemTree;

pub(crate) fn sample_tree() -> MemTree {
    let mut t = MemTree::new();
    t.insert(
        "toolchains.toml",
        "schema = 1\n[kind.git_bash]\ntier = \"base\"\ninstaller = \"base\"\ncheck = { path_from = \"git\" }\nversion_any = true\nprobe = { run = [\"bash\", \"--version\"], expect = \"GNU bash\" }\n",
    );
    t.insert(
        "projects/demo/project.toml",
        "schema = 1\nid = \"demo\"\nrepo = \"https://example.invalid/demo.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n",
    );
    t.insert(
        "projects/demo/recipe.toml",
        "schema = 1\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"cmd\", \"/c\", \"echo\", \"hi\"]\n\n[[step]]\nid = \"test\"\nkind = \"test\"\nrun = [\"cmd\", \"/c\", \"echo\", \"t\"]\nuncounted = true\n",
    );
    t
}

fn setup() -> (tempfile::TempDir, RecipeStore, Store, String) {
    let dir = tempfile::tempdir().unwrap();
    let rs = RecipeStore::open(&dir.path().join("recipes.git"), "main", "recipe/").unwrap();
    let store = Store::open_in_memory().unwrap();
    let main = rs.import_init(&sample_tree(), "admin-a", "처음 내용").unwrap();
    (dir, rs, store, main)
}

fn change(path: &str, content: &str) -> Change {
    Change { path: path.into(), content: Some(content.into()), delete: false, bytes: None }
}

#[test]
fn import_init_once_and_checked() {
    let (dir, rs, _store, main) = setup();
    assert_eq!(rs.main_commit().unwrap(), main);
    let e = rs.import_init(&sample_tree(), "a", "again").unwrap_err();
    assert_eq!(e.code, "main_exists");
    let rs2 = RecipeStore::open(&dir.path().join("other.git"), "main", "recipe/").unwrap();
    let mut bad = sample_tree();
    bad.insert("projects/demo/recipe.toml", "schema = 1\n[[step]]\nid = \"x\"\nkind = \"build\"\nrun = [\"a\"]\ntimeot_min = 1\n");
    let e = rs2.import_init(&bad, "a", "bad").unwrap_err();
    assert_eq!(e.code, "recipe_invalid");
    assert!(rs2.main_commit().is_none());
    let t = rs.tree(&main).unwrap();
    assert!(t.read("projects/demo/recipe.toml").is_some());
}

#[test]
fn edit_multi_file_conflict_and_main_guard() {
    let (_dir, rs, store, main) = setup();
    let e = rs.edit(&store, "u", "main", &main, "m", &[change("a.txt", "x")]).unwrap_err();
    assert_eq!(e.code, "main_not_editable");
    let r = rs
        .edit(&store, "member-b", "recipe/rename", &main, "단계 이름 바꾸기", &[
            change("projects/demo/recipe.toml", "schema = 1\n[[step]]\nid = \"compile\"\nkind = \"build\"\nrun = [\"cmd\"]\n"),
            change("projects/demo/notes.md", "renamed build → compile"),
        ])
        .unwrap();
    assert!(r.proposal_created);
    let p = store.proposal(r.proposal_id).unwrap().unwrap();
    assert_eq!(p.author, "member-b");
    assert_eq!(p.title, "단계 이름 바꾸기");
    // stale base
    let e = rs.edit(&store, "x", "recipe/rename", &main, "again", &[change("b.txt", "y")]).unwrap_err();
    assert_eq!(e.code, "recipe_conflict");
    // correct base works and does not open a second proposal
    let r2 = rs.edit(&store, "x", "recipe/rename", &r.commit, "more", &[change("b.txt", "y")]).unwrap();
    assert_eq!(r2.proposal_id, r.proposal_id);
    assert!(!r2.proposal_created);
    // author recorded on the commit
    let h = rs.history(&r2.commit, None, 5).unwrap();
    assert_eq!(h[0].author, "x");
    assert_eq!(h[1].author, "member-b");
}

#[test]
fn edit_refuses_new_errors_only() {
    let (_dir, rs, store, main) = setup();
    let e = rs
        .edit(&store, "u", "recipe/bad", &main, "typo", &[change("projects/demo/recipe.toml", "schema = 1\n[[step]]\nid = \"x\"\nkind = \"build\"\nrun = [\"a\"]\ntimeot_min = 1\n")])
        .unwrap_err();
    assert_eq!(e.code, "recipe_invalid");
    assert!(e.diags.iter().any(|d| d.message.contains("timeot_min")));
    assert!(rs.branch_head("recipe/bad").is_none(), "nothing saved");
}

#[test]
fn promote_fast_forward_only_with_head() {
    let (_dir, rs, store, main) = setup();
    let a = rs.edit(&store, "u", "recipe/a", &main, "a", &[change("a.txt", "1")]).unwrap();
    let b = rs.edit(&store, "u", "recipe/b", &main, "b", &[change("b.txt", "1")]).unwrap();
    // wrong head
    let e = rs.promote(&store, "admin", a.proposal_id, &main, None, |_, _| Ok(())).unwrap_err();
    assert_eq!(e.code, "recipe_conflict");
    // conditions veto
    let e = rs.promote(&store, "admin", a.proposal_id, &a.commit, None, |_, _| Err(RsError::new("promote_conditions_unmet", "no trial"))).unwrap_err();
    assert_eq!(e.code, "promote_conditions_unmet");
    let merged = rs.promote(&store, "admin", a.proposal_id, &a.commit, None, |_, _| Ok(())).unwrap();
    assert_eq!(rs.main_commit().unwrap(), merged);
    assert!(rs.branch_head("recipe/a").is_none());
    assert_eq!(store.proposal(a.proposal_id).unwrap().unwrap().status, "promoted");
    // b is now behind main
    let e = rs.promote(&store, "admin", b.proposal_id, &b.commit, None, |_, _| Ok(())).unwrap_err();
    assert_eq!(e.code, "not_fast_forward");
    // sync without conflicts, then promote
    let out = rs.sync(&store, "u", "recipe/b", &b.commit, None, &HashMap::new()).unwrap();
    let SyncOutcome::Merged { commit } = out else { panic!("{out:?}") };
    // a merge commit cannot be reverted (it would undo what came in from main)
    rs.promote(&store, "admin", b.proposal_id, &commit, None, |_, _| Ok(())).unwrap();
    let e = rs.revert("admin", &commit, "merge 되돌리기 시도").unwrap_err();
    assert!(e.message.contains("merge"), "{e}");
    let t = rs.tree(&rs.main_commit().unwrap()).unwrap();
    assert!(t.read("a.txt").is_some() && t.read("b.txt").is_some());
    // promoting again is refused, and the branch name can be reused
    let e = rs.promote(&store, "admin", b.proposal_id, &commit, None, |_, _| Ok(())).unwrap_err();
    assert_eq!(e.code, "proposal_closed");
    let again = rs.edit(&store, "u", "recipe/b", &rs.main_commit().unwrap(), "again", &[change("c.txt", "1")]).unwrap();
    assert!(again.proposal_created);
}

#[test]
fn sync_conflicts_need_resolutions() {
    let (_dir, rs, store, main) = setup();
    let a = rs.edit(&store, "u", "recipe/a", &main, "a", &[change("f.txt", "A\n")]).unwrap();
    let b = rs.edit(&store, "u", "recipe/b", &main, "b", &[change("f.txt", "B\n")]).unwrap();
    rs.promote(&store, "admin", a.proposal_id, &a.commit, None, |_, _| Ok(())).unwrap();
    let out = rs.sync(&store, "u", "recipe/b", &b.commit, None, &HashMap::new()).unwrap();
    let SyncOutcome::Conflicts { main: at, files } = out else { panic!("{out:?}") };
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].main.as_deref(), Some("A\n"));
    assert_eq!(files[0].branch.as_deref(), Some("B\n"));
    assert_eq!(files[0].base, None);
    let mut res = HashMap::new();
    res.insert("f.txt".to_string(), "AB\n".to_string());
    // resolutions without the main they were written against are refused
    assert_eq!(rs.sync(&store, "u", "recipe/b", &b.commit, None, &res).unwrap_err().code, "invalid_param");
    // and a symbolic head is not a pin
    assert_eq!(rs.sync(&store, "u", "recipe/b", "recipe/b", Some(&at), &res).unwrap_err().code, "invalid_param");
    let out = rs.sync(&store, "u", "recipe/b", &b.commit, Some(&at), &res).unwrap();
    let SyncOutcome::Merged { commit } = out else { panic!("{out:?}") };
    assert_eq!(rs.tree(&commit).unwrap().read_str("f.txt"), Some("AB\n"));
}

#[test]
fn close_keeps_commits_and_frees_name() {
    let (_dir, rs, store, main) = setup();
    let a = rs.edit(&store, "u", "recipe/a", &main, "a", &[change("f.txt", "1")]).unwrap();
    rs.close(&store, "u", a.proposal_id, Some("not needed")).unwrap();
    assert!(rs.branch_head("recipe/a").is_none());
    assert_eq!(rs.git.ref_value(&format!("refs/cimon/closed/{}", cimon_core::proposal_label(a.proposal_id))), Some(a.commit.clone()));
    assert_eq!(rs.close(&store, "u", a.proposal_id, None).unwrap_err().code, "proposal_closed");
}

#[test]
fn revert_makes_checked_commit() {
    let (_dir, rs, store, main) = setup();
    let a = rs.edit(&store, "u", "recipe/a", &main, "add file", &[change("f.txt", "1")]).unwrap();
    let promoted = rs.promote(&store, "admin", a.proposal_id, &a.commit, None, |_, _| Ok(())).unwrap();
    assert_eq!(rs.revert("admin", &promoted, "").unwrap_err().code, "recipe_invalid");
    let new = rs.revert("admin", &promoted, "급한 복구").unwrap();
    assert!(rs.tree(&new).unwrap().read("f.txt").is_none());
    let h = rs.history("main", None, 1).unwrap();
    assert!(h[0].subject.starts_with("되돌리기"), "{:?}", h[0]);
    assert!(h[0].body.contains("사유: 급한 복구"));
}

#[test]
fn reconcile_after_crash() {
    let (_dir, rs, store, main) = setup();
    let a = rs.edit(&store, "u", "recipe/a", &main, "a", &[change("f.txt", "1")]).unwrap();
    // simulate a crash right after main moved
    rs.git.update_ref(&rs.main_ref(), &a.commit, Some(&main), "test").unwrap();
    let b = rs.edit(&store, "u", "recipe/b", &a.commit, "b", &[change("g.txt", "1")]).unwrap();
    rs.git.delete_ref("refs/heads/recipe/b", &b.commit).unwrap();
    let notes = rs.reconcile(&store).unwrap();
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert_eq!(store.proposal(a.proposal_id).unwrap().unwrap().status, "promoted");
    assert_eq!(store.proposal(b.proposal_id).unwrap().unwrap().status, "closed");
    assert!(rs.branch_head("recipe/a").is_none());
}

#[test]
fn bundle_and_restore() {
    let (dir, rs, store, main) = setup();
    rs.edit(&store, "u", "recipe/a", &main, "a", &[change("f.txt", "1")]).unwrap();
    let b = dir.path().join("backup").join("r.bundle");
    rs.bundle(&b).unwrap();
    let restored = dir.path().join("restored.git");
    RecipeStore::restore(&b, &restored).unwrap();
    let rs2 = RecipeStore::open(&restored, "main", "recipe/").unwrap();
    assert_eq!(rs2.main_commit().unwrap(), main);
    assert!(rs2.branch_head("recipe/a").is_some());
    assert_eq!(RecipeStore::restore(&b, &restored).unwrap_err().code, "exists");
}

#[test]
fn resolve_and_show() {
    let (dir, rs, store, main) = setup();
    assert_eq!(rs.resolve("main").unwrap(), main);
    assert_eq!(rs.resolve(&main[..10]).unwrap(), main);
    assert_eq!(rs.resolve("recipe/none").unwrap_err().code, "unknown_recipe_ref");
    assert_eq!(rs.resolve("feature/x").unwrap_err().code, "trial_ref_not_allowed");
    match rs.show(&main, "projects").unwrap() {
        ShowResult::Dir { entries, .. } => assert_eq!(entries, vec!["demo/"]),
        other => panic!("{other:?}"),
    }
    assert!(rs.show(&main, "nope").is_err());
    let co = rs.checkout(&main, &dir.path().join("co").join(&main)).unwrap();
    assert!(co.join("projects/demo/recipe.toml").exists());
    let _ = store;
}

#[test]
fn windows_aliases_and_branch_case_are_refused() {
    let (_dir, rs, store, main) = setup();
    // a trailing dot names the same file as the reviewed one on NTFS
    let e = rs.edit(&store, "u", "recipe/alias", &main, "x", &[change("scripts/judge.py.", "evil")]).unwrap_err();
    assert_eq!(e.code, "recipe_invalid");
    let e = rs.edit(&store, "u", "recipe/alias", &main, "x", &[change("scripts/NUL", "x")]).unwrap_err();
    assert_eq!(e.code, "recipe_invalid");
    rs.edit(&store, "u", "recipe/alias", &main, "x", &[change("scripts/Run.ps1", "a")]).unwrap();
    let head = rs.branch_head("recipe/alias").unwrap();
    let e = rs.edit(&store, "u", "recipe/alias", &head, "x", &[change("scripts/run.ps1", "b")]).unwrap_err();
    assert!(e.message.contains("same path on Windows"), "{e}");
    // branch names are lower-case only, pins are commit ids only
    assert_eq!(rs.edit(&store, "u", "recipe/Alias", &main, "x", &[change("y", "1")]).unwrap_err().code, "trial_ref_not_allowed");
    assert_eq!(rs.edit(&store, "u", "recipe/other", "main", "x", &[change("y", "1")]).unwrap_err().code, "invalid_param");
}

#[test]
fn promote_intent_survives_and_revert_edges() {
    let (dir, rs, store, main) = setup();
    let a = rs.edit(&store, "u", "recipe/a", &main, "a", &[change("a.txt", "1")]).unwrap();
    // intent recorded, then (as if the process died) main moved without mark_promoted
    store.set_promote_intent(a.proposal_id, &a.commit, "admin-x", Some("급한 반영 — 시도 생략")).unwrap();
    rs.git.update_ref(&rs.main_ref(), &a.commit, Some(&main), "test").unwrap();
    rs.reconcile(&store).unwrap();
    let p = store.proposal(a.proposal_id).unwrap().unwrap();
    assert_eq!(p.status, "promoted");
    assert_eq!(p.promoted_by.as_deref(), Some("admin-x"));
    assert_eq!(p.override_reason.as_deref(), Some("급한 반영 — 시도 생략"));
    // reverting twice: the second would be an empty commit
    rs.revert("admin", &a.commit, "a.txt 를 되돌린다").unwrap();
    let e = rs.revert("admin", &a.commit, "a.txt 를 다시 되돌린다").unwrap_err();
    assert!(e.message.contains("already gone"), "{e}");
    // a failed restore leaves nothing behind, so a retry is possible
    let target = dir.path().join("restored");
    assert!(RecipeStore::restore(&dir.path().join("missing.bundle"), &target).is_err());
    assert!(!target.join("HEAD").exists());
}

#[test]
fn import_branch_keeps_bytes() {
    let (_dir, rs, store, _main) = setup();
    let mut t = (*rs.tree(&rs.main_commit().unwrap()).unwrap()).clone();
    let utf16: Vec<u8> = vec![0xFF, 0xFE, b'a', 0, b'b', 0];
    t.files.insert("scripts/x.ps1".into(), utf16.clone());
    let r = rs.import_branch(&store, "u", "recipe/bytes", &t, "utf-16 script").unwrap();
    assert_eq!(rs.tree(&r.commit).unwrap().read("scripts/x.ps1"), Some(utf16.as_slice()));
}
