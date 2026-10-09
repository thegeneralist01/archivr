//! Router-level tests: an entry hidden from a role (via its collection membership bits) is
//! hidden from every by-uid endpoint too, not just from the list and search queries.
//! Hidden entries are indistinguishable from missing ones (404).
//!
//! Cast: `admin` (sees everything), `alice` (plain user), `carol` (user + custom role
//! `editors`, bit 16). Two text entries are captured by the admin; `Hidden note`'s
//! membership is narrowed to the `editors` bit, so alice cannot see it and carol can.

use archivr_core::database;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::test_support::{
    Fixture, admin_session, fixture, guest_session, make_role_session, send, send_json,
    text_capture, wait_job,
};

const EDITORS_BIT: u32 = 16;

struct World {
    f: Fixture,
    admin: String,
    alice: String,
    carol: String,
    open: String,
    hidden: String,
    open_sha: String,
    hidden_sha: String,
}

fn entry_uid_by_title(f: &Fixture, title: &str) -> String {
    let conn = database::open_or_initialize(&f.archive_path).unwrap();
    conn.query_row(
        "SELECT entry_uid FROM archived_entries WHERE title = ?1",
        [title],
        |r| r.get(0),
    )
    .unwrap()
}

fn blob_sha_of(f: &Fixture, entry_uid: &str) -> String {
    let conn = database::open_or_initialize(&f.archive_path).unwrap();
    conn.query_row(
        "SELECT b.sha256 FROM entry_artifacts a \
         JOIN blobs b ON b.id = a.blob_id \
         JOIN archived_entries e ON e.id = a.entry_id \
         WHERE e.entry_uid = ?1",
        [entry_uid],
        |r| r.get(0),
    )
    .unwrap()
}

async fn world() -> World {
    let f = fixture();
    let admin = admin_session(&f.auth_path);
    {
        let conn = database::open_auth_db(&f.auth_path).unwrap();
        let role = database::create_custom_role(&conn, "editors", "Editors").unwrap();
        assert_eq!(1u32 << role.bit_position, EDITORS_BIT);
    }
    let alice = make_role_session(&f.auth_path, "alice", &["user"]);
    let carol = make_role_session(&f.auth_path, "carol", &["user", "editors"]);

    text_capture(&f, &admin, "Open note").await;
    text_capture(&f, &admin, "Hidden note").await;
    let open = entry_uid_by_title(&f, "Open note");
    let hidden = entry_uid_by_title(&f, "Hidden note");
    {
        let conn = database::open_or_initialize(&f.archive_path).unwrap();
        conn.execute(
            "UPDATE collection_entries SET visibility_bits = ?1 \
             WHERE entry_id = (SELECT id FROM archived_entries WHERE entry_uid = ?2)",
            rusqlite::params![EDITORS_BIT as i64, hidden],
        )
        .unwrap();
    }
    let open_sha = blob_sha_of(&f, &open);
    let hidden_sha = blob_sha_of(&f, &hidden);
    World { f, admin, alice, carol, open, hidden, open_sha, hidden_sha }
}

fn entry_path(uid: &str, suffix: &str) -> String {
    format!("/api/archives/test/entries/{uid}{suffix}")
}

/// One by-uid request template: method, path suffix after the entry uid, JSON body, and the
/// status a caller who CAN see the entry gets.
type Case = (&'static str, &'static str, Option<Value>, StatusCode);

/// Endpoints that are safe to repeat against a visible entry (no destructive side effects).
fn safe_cases() -> Vec<Case> {
    vec![
        ("GET", "", None, StatusCode::OK),
        ("GET", "/summary", None, StatusCode::OK),
        ("GET", "/artifacts/0", None, StatusCode::OK),
        ("POST", "/artifacts/0/media-token", None, StatusCode::OK),
        ("GET", "/tags", None, StatusCode::OK),
        ("GET", "/collections", None, StatusCode::OK),
        // Provider "bogus" is a 400 once the entry gate has passed.
        (
            "POST",
            "/summary",
            Some(json!({"provider": "bogus"})),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/thread-title",
            Some(json!({"provider": "bogus"})),
            StatusCode::BAD_REQUEST,
        ),
    ]
}

#[tokio::test]
async fn hidden_entry_is_not_found_on_every_by_uid_endpoint() {
    let w = world().await;
    let hidden = &w.hidden;

    let mut cases = safe_cases();
    cases.extend([
        ("GET", "/favicon", None, StatusCode::NOT_FOUND),
        ("PATCH", "", Some(json!({"title": "pwned"})), StatusCode::NO_CONTENT),
        ("DELETE", "", None, StatusCode::NO_CONTENT),
        ("POST", "/tags", Some(json!({"tag_path": "/secret"})), StatusCode::CREATED),
        ("POST", "/rearchive", None, StatusCode::ACCEPTED),
    ]);
    for (method, suffix, body, _) in &cases {
        let uri = entry_path(hidden, suffix);
        let (status, json) = send_json(&w.f.router, method, &uri, Some(&w.alice), body.as_ref()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "alice {method} {uri}");
        assert_eq!(json["error"], "entry not found", "alice {method} {uri}");
    }

    // Nothing was changed behind the 404s.
    let conn = database::open_or_initialize(&w.f.archive_path).unwrap();
    let (title, tags): (String, i64) = conn
        .query_row(
            "SELECT title, (SELECT COUNT(*) FROM entry_tag_assignments) \
             FROM archived_entries WHERE entry_uid = ?1",
            [hidden],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("entry must still exist");
    assert_eq!(title, "Hidden note");
    assert_eq!(tags, 0);
    let jobs: i64 = conn
        .query_row("SELECT COUNT(*) FROM capture_jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(jobs, 2, "rearchive of a hidden entry must not create a job");

    // The tag-removal route is gated too (the tag need not exist for the gate to answer).
    let (status, json) = send_json(
        &w.f.router,
        "DELETE",
        &entry_path(hidden, "/tags/tag_x"),
        Some(&w.alice),
        None,
    )
    .await;
    assert_eq!((status, json["error"].as_str()), (StatusCode::NOT_FOUND, Some("entry not found")));
}

#[tokio::test]
async fn visible_entry_passes_the_gate() {
    let w = world().await;
    // A plain user on the open entry, the editor and the admin on the hidden one.
    for (who, cookie, uid) in [
        ("alice/open", &w.alice, &w.open),
        ("carol/hidden", &w.carol, &w.hidden),
        ("admin/hidden", &w.admin, &w.hidden),
        ("admin/open", &w.admin, &w.open),
    ] {
        for (method, suffix, body, expected) in safe_cases() {
            let uri = entry_path(uid, suffix);
            let (status, json) = send_json(&w.f.router, method, &uri, Some(cookie), body.as_ref()).await;
            assert_eq!(status, expected, "{who} {method} {uri}: {json}");
        }
    }
}

#[tokio::test]
async fn visible_entry_can_be_written_by_whoever_sees_it() {
    let w = world().await;
    // carol (editors) may rename and tag the hidden entry; alice may do the same to the open one.
    for (cookie, uid, title) in [(&w.carol, &w.hidden, "Renamed by carol"), (&w.alice, &w.open, "Renamed by alice")] {
        let (status, _) = send_json(
            &w.f.router,
            "PATCH",
            &entry_path(uid, ""),
            Some(cookie),
            Some(&json!({"title": title})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = send_json(
            &w.f.router,
            "POST",
            &entry_path(uid, "/tags"),
            Some(cookie),
            Some(&json!({"tag_path": "/mine"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    // Rearchive passes the gate for the editor (the background job then fails: not a tweet).
    let (status, body) =
        send_json(&w.f.router, "POST", &entry_path(&w.hidden, "/rearchive"), Some(&w.carol), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    wait_job(&w.f.archive_path, body["job_uid"].as_str().unwrap()).await;
}

#[tokio::test]
async fn hidden_entry_cannot_be_deleted_by_a_plain_user_but_can_by_the_editor() {
    let w = world().await;
    let (status, _) = send(&w.f.router, "DELETE", &entry_path(&w.hidden, ""), Some(&w.alice), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&w.f.router, "DELETE", &entry_path(&w.hidden, ""), Some(&w.carol), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&w.f.router, "GET", &entry_path(&w.hidden, ""), Some(&w.admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the entry is really gone");
}

#[tokio::test]
async fn blobs_follow_the_entries_that_use_them() {
    let w = world().await;
    let blob = |sha: &str| format!("/api/archives/test/blobs/{sha}");

    let (status, body) = send(&w.f.router, "GET", &blob(&w.open_sha), Some(&w.alice), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(String::from_utf8_lossy(&body), "body of Open note");

    let (status, body) = send_json(&w.f.router, "GET", &blob(&w.hidden_sha), Some(&w.alice), None).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::NOT_FOUND, Some("blob not found")));

    for cookie in [&w.carol, &w.admin] {
        let (status, body) = send(&w.f.router, "GET", &blob(&w.hidden_sha), Some(cookie), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(String::from_utf8_lossy(&body), "body of Hidden note");
    }

    // Once the open entry also uses the hidden entry's blob, alice may read it: she can see
    // an entry that references it.
    {
        let conn = database::open_or_initialize(&w.f.archive_path).unwrap();
        conn.execute(
            "UPDATE entry_artifacts SET blob_id = (SELECT id FROM blobs WHERE sha256 = ?1) \
             WHERE entry_id = (SELECT id FROM archived_entries WHERE entry_uid = ?2)",
            rusqlite::params![w.hidden_sha, w.open],
        )
        .unwrap();
    }
    let (status, _) = send(&w.f.router, "GET", &blob(&w.hidden_sha), Some(&w.alice), None).await;
    assert_eq!(status, StatusCode::OK);

    // A blob no entry references is admin-only; unknown blobs are 404 for everyone.
    {
        let conn = database::open_or_initialize(&w.f.archive_path).unwrap();
        conn.execute("DELETE FROM entry_artifacts", []).unwrap();
    }
    let (status, _) = send(&w.f.router, "GET", &blob(&w.hidden_sha), Some(&w.alice), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&w.f.router, "GET", &blob("nope"), Some(&w.admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn media_tokens_are_only_issued_for_visible_entries() {
    let w = world().await;
    let issue = |cookie: &str| {
        let uri = entry_path(&w.hidden, "/artifacts/0/media-token");
        let router = w.f.router.clone();
        let cookie = cookie.to_string();
        async move { send_json(&router, "POST", &uri, Some(&cookie), None).await }
    };
    let (status, _) = issue(&w.alice).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, issued) = issue(&w.carol).await;
    assert_eq!(status, StatusCode::OK);
    // The token is a capability URL: it works without a session...
    let url = issued["url"].as_str().unwrap();
    let (status, body) = send(&w.f.router, "GET", url, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(String::from_utf8_lossy(&body), "body of Hidden note");
    // ...but a session without access gets nothing from the plain URL.
    let plain = entry_path(&w.hidden, "/artifacts/0");
    let (status, _) = send(&w.f.router, "GET", &plain, Some(&w.alice), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn anonymous_requests_keep_the_existing_guest_rules() {
    let w = world().await;
    // The default collection requires auth, so nothing is public: the existing 401.
    for uid in [&w.open, &w.hidden] {
        let (status, _) = send(&w.f.router, "GET", &entry_path(uid, ""), None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_guest_role_account_sees_only_entries_shared_with_the_guest_bit() {
    let w = world().await;
    // A signed-in account holding only the guest role has role bits 1: it is an
    // authenticated caller, so the same visibility rule as everyone else applies.
    let guest = guest_session(&w.f.auth_path);
    for uid in [&w.open, &w.hidden] {
        let (status, _) = send(&w.f.router, "GET", &entry_path(uid, ""), Some(&guest), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "bits 2 / 16 do not overlap the guest bit");
    }
    {
        let conn = database::open_or_initialize(&w.f.archive_path).unwrap();
        conn.execute(
            "UPDATE collection_entries SET visibility_bits = 3 \
             WHERE entry_id = (SELECT id FROM archived_entries WHERE entry_uid = ?1)",
            [&w.open],
        )
        .unwrap();
    }
    let (status, _) = send(&w.f.router, "GET", &entry_path(&w.open, ""), Some(&guest), None).await;
    assert_eq!(status, StatusCode::OK, "an entry shared with bit 1 is readable by guest accounts");
}

#[tokio::test]
async fn hidden_parent_lists_no_children_and_tag_counts_hide_it() {
    let w = world().await;
    // Children listing already filters per child; a hidden parent simply has none for alice.
    let (status, list) = send_json(&w.f.router, "GET", &entry_path(&w.hidden, "/children"), Some(&w.alice), None).await;
    assert_eq!((status, list), (StatusCode::OK, json!([])));

    // Tag both entries; the tag tree counts only what the caller can see.
    for uid in [&w.open, &w.hidden] {
        let (status, _) = send_json(
            &w.f.router,
            "POST",
            &entry_path(uid, "/tags"),
            Some(&w.admin),
            Some(&json!({"tag_path": "/shared"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let count_for = |cookie: String| {
        let router = w.f.router.clone();
        async move {
            let (status, tree) = send_json(&router, "GET", "/api/archives/test/tags", Some(&cookie), None).await;
            assert_eq!(status, StatusCode::OK);
            let node = tree
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["tag"]["slug"] == "shared")
                .expect("tag is listed for every logged-in user")
                .clone();
            (node["entry_count"].as_i64().unwrap(), node["subtree_count"].as_i64().unwrap())
        }
    };
    assert_eq!(count_for(w.alice.clone()).await, (1, 1));
    assert_eq!(count_for(w.carol.clone()).await, (2, 2));
    assert_eq!(count_for(w.admin.clone()).await, (2, 2));
}

#[tokio::test]
async fn collection_membership_endpoints_are_gated_by_entry_visibility() {
    let w = world().await;
    let (status, created) = send_json(
        &w.f.router,
        "POST",
        "/api/archives/test/collections",
        Some(&w.alice),
        Some(&json!({"name": "Shelf", "slug": "shelf"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let coll = created["collection_uid"].as_str().unwrap().to_string();
    let members = format!("/api/archives/test/collections/{coll}/entries");

    // alice: the open entry works, the hidden one is not found.
    let (status, _) = send_json(
        &w.f.router, "POST", &members, Some(&w.alice),
        Some(&json!({"entry_uid": w.open, "visibility_bits": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send_json(
        &w.f.router, "POST", &members, Some(&w.alice),
        Some(&json!({"entry_uid": w.hidden, "visibility_bits": 2})),
    )
    .await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::NOT_FOUND, Some("entry not found")));
    for (method, body) in [("PATCH", Some(json!({"visibility_bits": 3}))), ("DELETE", None)] {
        let uri = format!("{members}/{}", w.hidden);
        let (status, _) = send_json(&w.f.router, method, &uri, Some(&w.alice), body.as_ref()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "alice {method} {uri}");
    }

    // carol can manage the hidden entry's membership.
    let (status, _) = send_json(
        &w.f.router, "POST", &members, Some(&w.carol),
        Some(&json!({"entry_uid": w.hidden, "visibility_bits": EDITORS_BIT})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let uri = format!("{members}/{}", w.hidden);
    let (status, _) = send_json(&w.f.router, "DELETE", &uri, Some(&w.carol), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
