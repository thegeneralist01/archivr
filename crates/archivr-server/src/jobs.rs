//! Capture-job endpoints that are new paths (J1: the job collection) plus the helpers the
//! in-place handlers in `routes.rs` share (J2 detail, caller identity, query parsing).
//!
//! Visibility rule: a caller sees a job if they created it (`capture_jobs.created_by`
//! equals their auth-DB `user_uid`) or hold ADMIN. Rows with `created_by IS NULL`
//! (legacy / CLI jobs) are therefore admin-only.
use std::path::Path as FsPath;

use archivr_core::{archive, capture_jobs, database};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};

use crate::routes::{
    ApiError, AppState, AuthUser, ROLE_ADMIN, ROLE_USER, mounted_archive,
};

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/archives/:archive_id/capture_jobs",
        get(list_capture_jobs_handler),
    )
}

/// Resolves the caller's auth-DB `user_uid` (sessions and Bearer tokens both map to a
/// user). `None` for guests or an unknown user id.
pub(crate) fn caller_user_uid(
    state: &AppState,
    auth: &AuthUser,
) -> Result<Option<String>, ApiError> {
    match auth {
        AuthUser::Authenticated { user_id, .. } => {
            let conn = database::open_auth_db(&state.auth_db_path)?;
            Ok(database::get_user_uid(&conn, *user_id)?)
        }
        AuthUser::Guest => Ok(None),
    }
}

/// Parses an optional integer query parameter; non-integers are a 400 (JSON error body).
pub(crate) fn parse_int_param(name: &str, raw: Option<&str>) -> Result<Option<i64>, ApiError> {
    match raw {
        None => Ok(None),
        Some(s) => s
            .trim()
            .parse::<i64>()
            .map(Some)
            .map_err(|_| ApiError::bad_request(&format!("{name} must be an integer"))),
    }
}

#[derive(Debug, serde::Deserialize, Default)]
struct JobListQuery {
    status: Option<String>,
    limit: Option<String>,
    offset: Option<String>,
    created_by: Option<String>,
}

/// `GET /api/archives/:archive_id/capture_jobs` (J1).
async fn list_capture_jobs_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(archive_id): Path<String>,
    Query(q): Query<JobListQuery>,
) -> Result<Json<Vec<archive::CaptureJobSummary>>, ApiError> {
    auth.require_role(ROLE_USER)?;
    if let Some(status) = q.status.as_deref()
        && !capture_jobs::JOB_STATUSES.contains(&status)
    {
        return Err(ApiError::bad_request(
            "invalid status: must be pending, running, completed, or failed",
        ));
    }
    let limit = parse_int_param("limit", q.limit.as_deref())?
        .unwrap_or(capture_jobs::JOB_LIST_DEFAULT_LIMIT);
    let offset = parse_int_param("offset", q.offset.as_deref())?.unwrap_or(0);
    let mounted = mounted_archive(&state, &archive_id)?;
    let caller_uid = caller_user_uid(&state, &auth)?
        .ok_or_else(|| ApiError::unauthorized("login required"))?;
    let is_admin = auth.has_role(ROLE_ADMIN);

    // `None` = no owner filter (admin only); a non-admin is always scoped to themselves.
    let owner: Option<String> = match q.created_by.as_deref() {
        None | Some("all") if is_admin => None,
        None | Some("all") | Some("me") => Some(caller_uid),
        Some(uid) if uid == caller_uid => Some(caller_uid),
        Some(uid) if is_admin => Some(uid.to_string()),
        Some(_) => {
            return Err(ApiError::forbidden(
                "you can only list your own capture jobs",
            ));
        }
    };

    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(capture_jobs::list_capture_jobs(
        &conn,
        q.status.as_deref(),
        owner.as_deref(),
        limit,
        offset,
    )?))
}

/// `GET .../capture_jobs/:job_uid` response (J2): the existing job fields plus the
/// produced entries and per-item outcomes of the job's run.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CaptureJobDetail {
    #[serde(flatten)]
    job: archive::CaptureJobSummary,
    entry_uids: Vec<String>,
    items: Vec<capture_jobs::JobItem>,
    items_truncated: bool,
}

/// Loads a job for `GET .../capture_jobs/:job_uid`. A caller who neither created the job
/// nor holds ADMIN gets 404, so the job's existence is not disclosed.
pub(crate) fn get_job_detail(
    state: &AppState,
    auth: &AuthUser,
    archive_path: &FsPath,
    job_uid: &str,
) -> Result<CaptureJobDetail, ApiError> {
    let conn = database::open_or_initialize(archive_path)?;
    let job = archive::get_capture_job(&conn, job_uid)?
        .ok_or_else(|| ApiError::not_found("capture job not found"))?;
    if !auth.has_role(ROLE_ADMIN) {
        let caller_uid = caller_user_uid(state, auth)?;
        if caller_uid.is_none() || job.created_by != caller_uid {
            return Err(ApiError::not_found("capture job not found"));
        }
    }
    let items = match job.run_uid.as_deref() {
        Some(run_uid) => capture_jobs::list_job_items(&conn, run_uid)?,
        None => capture_jobs::JobItems {
            entry_uids: Vec::new(),
            items: Vec::new(),
            items_truncated: false,
        },
    };
    Ok(CaptureJobDetail {
        job,
        entry_uids: items.entry_uids,
        items: items.items,
        items_truncated: items.items_truncated,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path as FsPath;

    use archivr_core::database;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::test_support::{
        Fixture, admin_session, body_json, fixture, get, guest_session, make_api_token,
        make_role_session, make_test_session, post_json, text_capture, user_uid, wait_job,
    };

    fn uids(list: &Value) -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|j| j["job_uid"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn job_list_requires_user_role() {
        let f = fixture();
        let (s, _) = get(&f.router, "/api/archives/test/capture_jobs", None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let guest = guest_session(&f.auth_path);
        let (s, _) = get(&f.router, "/api/archives/test/capture_jobs", Some(&guest)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = get(
            &f.router,
            "/api/archives/nope/capture_jobs",
            Some(&make_test_session(&f.auth_path)),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn users_only_see_their_own_jobs_and_admin_sees_all() {
        let f = fixture();
        let alice = make_role_session(&f.auth_path, "alice", &["user"]);
        let bob = make_role_session(&f.auth_path, "bob", &["user"]);
        let admin = admin_session(&f.auth_path);
        let alice_uid = user_uid(&f.auth_path, "alice");
        let bob_uid = user_uid(&f.auth_path, "bob");

        let alice_job = text_capture(&f, &alice, "alice note").await;
        let bob_job = text_capture(&f, &bob, "bob note").await;
        let legacy_job = {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            database::create_capture_job(&conn, "test").unwrap()
        };

        let (s, list) = get(&f.router, "/api/archives/test/capture_jobs", Some(&alice)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(uids(&list), vec![alice_job.clone()]);
        assert_eq!(list[0]["created_by"], alice_uid);
        let (_, list) = get(&f.router, "/api/archives/test/capture_jobs", Some(&bob)).await;
        assert_eq!(uids(&list), vec![bob_job.clone()]);

        // GET by uid: someone else's job and NULL-owner jobs are 404, not 403.
        let base = "/api/archives/test/capture_jobs";
        let (s, _) = get(&f.router, &format!("{base}/{bob_job}"), Some(&alice)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = get(&f.router, &format!("{base}/{legacy_job}"), Some(&alice)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, own) = get(&f.router, &format!("{base}/{alice_job}"), Some(&alice)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(own["created_by"], alice_uid);

        // Admin sees everything, including the NULL-owner row.
        let (s, all) = get(&f.router, base, Some(&admin)).await;
        assert_eq!(s, StatusCode::OK);
        let all = uids(&all);
        for j in [&alice_job, &bob_job, &legacy_job] {
            assert!(all.contains(j), "admin should see {j}");
        }
        for j in [&alice_job, &bob_job, &legacy_job] {
            let (s, _) = get(&f.router, &format!("{base}/{j}"), Some(&admin)).await;
            assert_eq!(s, StatusCode::OK);
        }
        let (_, mine) = get(&f.router, &format!("{base}?created_by=me"), Some(&admin)).await;
        assert!(uids(&mine).is_empty());
        let (_, bobs) = get(
            &f.router,
            &format!("{base}?created_by={bob_uid}"),
            Some(&admin),
        )
        .await;
        assert_eq!(uids(&bobs), vec![bob_job.clone()]);

        // Non-admins may only ask for themselves.
        let (s, _) = get(
            &f.router,
            &format!("{base}?created_by={bob_uid}"),
            Some(&alice),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, l) = get(&f.router, &format!("{base}?created_by=me"), Some(&alice)).await;
        assert_eq!((s, uids(&l)), (StatusCode::OK, vec![alice_job.clone()]));
        let (s, l) = get(&f.router, &format!("{base}?created_by=all"), Some(&alice)).await;
        assert_eq!((s, uids(&l)), (StatusCode::OK, vec![alice_job.clone()]));
        let (s, l) = get(
            &f.router,
            &format!("{base}?created_by={alice_uid}"),
            Some(&alice),
        )
        .await;
        assert_eq!((s, uids(&l)), (StatusCode::OK, vec![alice_job]));
    }

    #[tokio::test]
    async fn job_list_status_limit_and_offset() {
        let f = fixture();
        let alice = make_role_session(&f.auth_path, "alice", &["user"]);
        let alice_uid = user_uid(&f.auth_path, "alice");
        let (first, second, failed) = {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            let a = database::create_capture_job_as(&conn, "test", Some(&alice_uid)).unwrap();
            let b = database::create_capture_job_as(&conn, "test", Some(&alice_uid)).unwrap();
            let c = database::create_capture_job_as(&conn, "test", Some(&alice_uid)).unwrap();
            database::update_capture_job_status(&conn, &c, "failed", None, Some("boom"), None)
                .unwrap();
            (a, b, c)
        };
        let base = "/api/archives/test/capture_jobs";

        let (s, l) = get(&f.router, base, Some(&alice)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(uids(&l), vec![failed.clone(), second.clone(), first.clone()]);

        let (_, l) = get(&f.router, &format!("{base}?status=failed"), Some(&alice)).await;
        assert_eq!(uids(&l), vec![failed.clone()]);
        assert_eq!(l[0]["error_text"], "boom");
        let (_, l) = get(&f.router, &format!("{base}?status=pending"), Some(&alice)).await;
        assert_eq!(uids(&l), vec![second.clone(), first.clone()]);

        let (_, l) = get(&f.router, &format!("{base}?limit=1"), Some(&alice)).await;
        assert_eq!(uids(&l), vec![failed.clone()]);
        let (_, l) = get(&f.router, &format!("{base}?limit=1&offset=1"), Some(&alice)).await;
        assert_eq!(uids(&l), vec![second]);
        let (s, l) = get(&f.router, &format!("{base}?limit=100000"), Some(&alice)).await;
        assert_eq!((s, l.as_array().unwrap().len()), (StatusCode::OK, 3));

        for bad in ["status=bogus", "limit=abc", "offset=x"] {
            let (s, body) = get(&f.router, &format!("{base}?{bad}"), Some(&alice)).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
            assert!(body["error"].is_string());
        }
    }

    #[tokio::test]
    async fn token_and_session_callers_are_recorded_as_job_creators() {
        let f = fixture();
        let _ = make_role_session(&f.auth_path, "carol", &["user"]);
        let carol_uid = user_uid(&f.auth_path, "carol");
        let token = make_api_token(&f.auth_path, "carol", None, "full");

        let (s, body) = post_json(
            &f.router,
            "/api/archives/test/captures/text",
            ("authorization", &format!("Bearer {token}")),
            &json!({"title": "t", "body": "b", "mime": "text/plain"}),
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);
        let job_uid = body["job_uid"].as_str().unwrap();
        wait_job(&f.archive_path, job_uid).await;

        // Rearchive records its job (a non-tweet entry then fails in the background), but an
        // unknown entry is a 404 and leaves no job behind.
        let dave = make_role_session(&f.auth_path, "dave", &["user"]);
        let dave_uid = user_uid(&f.auth_path, "dave");
        let jobs_before = {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            conn.query_row("SELECT COUNT(*) FROM capture_jobs", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let (s, _) = post_json(
            &f.router,
            "/api/archives/test/entries/ent_missing/rearchive",
            ("cookie", &dave),
            &json!({}),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let entry_uid: String = {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM capture_jobs", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                jobs_before,
                "a 404 rearchive must not create a job"
            );
            conn.query_row("SELECT entry_uid FROM archived_entries LIMIT 1", [], |r| r.get(0))
                .unwrap()
        };
        let (s, body) = post_json(
            &f.router,
            &format!("/api/archives/test/entries/{entry_uid}/rearchive"),
            ("cookie", &dave),
            &json!({}),
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);
        let rearchive_job = body["job_uid"].as_str().unwrap();

        let conn = database::open_or_initialize(&f.archive_path).unwrap();
        let by = |uid: &str| database::get_capture_job(&conn, uid).unwrap().unwrap().created_by;
        assert_eq!(by(job_uid), Some(carol_uid));
        assert_eq!(by(rearchive_job), Some(dave_uid));
    }

    #[tokio::test]
    async fn job_detail_has_entry_uids_and_items_after_text_capture() {
        let f = fixture();
        let alice = make_role_session(&f.auth_path, "alice", &["user"]);
        let job_uid = text_capture(&f, &alice, "detail note").await;

        let (s, job) = get(
            &f.router,
            &format!("/api/archives/test/capture_jobs/{job_uid}"),
            Some(&alice),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        // Existing fields unchanged.
        assert_eq!(job["job_uid"], job_uid);
        assert_eq!(job["archive_id"], "test");
        assert_eq!(job["status"], "completed");
        assert!(job["run_uid"].is_string());
        for key in ["error_text", "notes_json", "created_at", "updated_at"] {
            assert!(job.get(key).is_some(), "{key} must stay present");
        }
        assert_eq!(job["created_by"], user_uid(&f.auth_path, "alice"));

        let entry_uids = job["entry_uids"].as_array().unwrap();
        assert_eq!(entry_uids.len(), 1);
        let items = job["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["entry_uid"], entry_uids[0]);
        assert_eq!(items[0]["status"], "completed");
        assert!(items[0]["requested_locator"].is_string());
        assert!(items[0].get("error_text").is_some());
        assert_eq!(job["items_truncated"], false);

        // The entry really exists.
        let (s, _) = get(
            &f.router,
            &format!(
                "/api/archives/test/entries/{}",
                entry_uids[0].as_str().unwrap()
            ),
            Some(&alice),
        )
        .await;
        assert_eq!(s, StatusCode::OK);

        // A job whose run has not been linked yet has empty lists.
        let pending = {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            database::create_capture_job_as(
                &conn,
                "test",
                Some(&user_uid(&f.auth_path, "alice")),
            )
            .unwrap()
        };
        let (s, p) = get(
            &f.router,
            &format!("/api/archives/test/capture_jobs/{pending}"),
            Some(&alice),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(p["entry_uids"], json!([]));
        assert_eq!(p["items"], json!([]));
    }

    #[tokio::test]
    async fn job_detail_requires_user_role() {
        let f = fixture();
        let (s, _) = get(&f.router, "/api/archives/test/capture_jobs/job_x", None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let guest = guest_session(&f.auth_path);
        let (s, _) = get(
            &f.router,
            "/api/archives/test/capture_jobs/job_x",
            Some(&guest),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let admin = admin_session(&f.auth_path);
        let (s, _) = get(
            &f.router,
            "/api/archives/test/capture_jobs/job_x",
            Some(&admin),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    fn hide_all_entries_from_users(archive_path: &FsPath) {
        let conn = database::open_or_initialize(archive_path).unwrap();
        // ADMIN|OWNER only
        conn.execute("UPDATE collection_entries SET visibility_bits = 12", [])
            .unwrap();
    }

    fn run_uids(list: &Value) -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|r| r["run_uid"].as_str().unwrap().to_string())
            .collect()
    }

    fn run_of(archive_path: &FsPath, job_uid: &str) -> String {
        let conn = database::open_or_initialize(archive_path).unwrap();
        database::get_capture_job(&conn, job_uid)
            .unwrap()
            .unwrap()
            .run_uid
            .unwrap()
    }

    #[tokio::test]
    async fn runs_are_visible_by_admin_creator_or_entry_access() {
        let f = fixture();
        let alice = make_role_session(&f.auth_path, "alice", &["user"]);
        let bob = make_role_session(&f.auth_path, "bob", &["user"]);
        let admin = admin_session(&f.auth_path);
        let runs = "/api/archives/test/runs";

        let (s, _) = get(&f.router, runs, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);

        let alice_run = run_of(&f.archive_path, &text_capture(&f, &alice, "a").await);
        let bob_run = run_of(&f.archive_path, &text_capture(&f, &bob, "b").await);

        // Entry-access path: default collection entries are visible to USER (bit 2), so
        // both users currently see both runs; the response is still a plain array.
        let (s, l) = get(&f.router, runs, Some(&alice)).await;
        assert_eq!(s, StatusCode::OK);
        let seen = run_uids(&l);
        assert!(seen.contains(&alice_run) && seen.contains(&bob_run));
        assert!(l[0].get("started_at").is_some() && l[0].get("error_summary").is_some());

        // Remove entry access: only the creator (and admins) see each run.
        hide_all_entries_from_users(&f.archive_path);
        let (_, l) = get(&f.router, runs, Some(&alice)).await;
        assert_eq!(run_uids(&l), vec![alice_run.clone()]);
        let (_, l) = get(&f.router, runs, Some(&bob)).await;
        assert_eq!(run_uids(&l), vec![bob_run.clone()]);
        let (_, l) = get(&f.router, runs, Some(&admin)).await;
        let seen = run_uids(&l);
        assert!(seen.contains(&alice_run) && seen.contains(&bob_run));

        // A third user with no job and no entry access sees nothing.
        let eve = make_role_session(&f.auth_path, "eve", &["user"]);
        let (s, l) = get(&f.router, runs, Some(&eve)).await;
        assert_eq!((s, run_uids(&l).len()), (StatusCode::OK, 0));

        // Entry access alone is enough: grant bit 2 again on Bob's entry only.
        {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            conn.execute(
                "UPDATE collection_entries SET visibility_bits = 2 WHERE entry_id IN (
                     SELECT i.produced_entry_id FROM archive_run_items i
                     JOIN archive_runs r ON r.id = i.run_id WHERE r.run_uid = ?1)",
                [&bob_run],
            )
            .unwrap();
        }
        let (_, l) = get(&f.router, runs, Some(&eve)).await;
        assert_eq!(run_uids(&l), vec![bob_run.clone()]);
        let (_, l) = get(&f.router, runs, Some(&alice)).await;
        let seen = run_uids(&l);
        assert!(seen.contains(&alice_run) && seen.contains(&bob_run));

        // Runs without a job row (pre-migration) follow entry access only.
        {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            conn.execute("DELETE FROM capture_jobs", []).unwrap();
        }
        let (_, l) = get(&f.router, runs, Some(&alice)).await;
        assert_eq!(run_uids(&l), vec![bob_run.clone()]);
        let (_, l) = get(&f.router, runs, Some(&admin)).await;
        assert_eq!(run_uids(&l).len(), 2);
    }

    #[tokio::test]
    async fn creator_sees_their_run_while_the_job_is_still_in_progress() {
        let f = fixture();
        let alice = make_role_session(&f.auth_path, "alice", &["user"]);
        let bob = make_role_session(&f.auth_path, "bob", &["user"]);
        let alice_uid = user_uid(&f.auth_path, "alice");

        // A job that created its run but has not finished: still `pending` in the jobs table.
        let job_uid = {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            database::create_capture_job_as(&conn, "test", Some(&alice_uid)).unwrap()
        };
        let paths = archivr_core::archive::read_archive_paths(&f.archive_path).unwrap();
        let result = archivr_core::capture::perform_text_capture_for_job(
            &paths, "In flight", "body", "text/plain", None, Some(&job_uid),
        )
        .unwrap();
        hide_all_entries_from_users(&f.archive_path);

        let runs = "/api/archives/test/runs";
        let (_, l) = get(&f.router, runs, Some(&alice)).await;
        assert_eq!(run_uids(&l), vec![result.run_uid.clone()], "the creator sees it at once");
        let (_, l) = get(&f.router, runs, Some(&bob)).await;
        assert!(run_uids(&l).is_empty(), "other users see neither the run nor its entry");
        let conn = database::open_or_initialize(&f.archive_path).unwrap();
        let job = database::get_capture_job(&conn, &job_uid).unwrap().unwrap();
        assert_eq!(job.status, "pending");
    }

    #[tokio::test]
    async fn runs_support_limit_and_offset() {
        let f = fixture();
        let admin = admin_session(&f.auth_path);
        let first = run_of(&f.archive_path, &text_capture(&f, &admin, "one").await);
        let second = run_of(&f.archive_path, &text_capture(&f, &admin, "two").await);
        let runs = "/api/archives/test/runs";

        let (_, all) = get(&f.router, runs, Some(&admin)).await;
        assert_eq!(run_uids(&all), vec![second.clone(), first.clone()]);
        let (_, l) = get(&f.router, &format!("{runs}?limit=1"), Some(&admin)).await;
        assert_eq!(run_uids(&l), vec![second]);
        let (_, l) = get(&f.router, &format!("{runs}?limit=1&offset=1"), Some(&admin)).await;
        assert_eq!(run_uids(&l), vec![first]);
        let (s, _) = get(&f.router, &format!("{runs}?limit=abc"), Some(&admin)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    fn multipart_upload(filename: &str, content: &str) -> Request<Body> {
        let body = format!(
            "--XBOUNDARY\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{filename}\"\r\nContent-Type: text/plain\r\n\r\n{content}\r\n--XBOUNDARY--\r\n"
        );
        Request::builder()
            .method("POST")
            .uri("/api/archives/test/uploads")
            .header("content-type", "multipart/form-data; boundary=XBOUNDARY")
            .body(Body::from(body))
            .unwrap()
    }

    async fn capture_locator(f: &Fixture, cookie: &str, locator: &str) -> (StatusCode, Value) {
        post_json(
            &f.router,
            "/api/archives/test/captures",
            ("cookie", cookie),
            &json!({"locator": locator}),
        )
        .await
    }

    #[tokio::test]
    async fn file_locators_must_reference_a_staged_upload() {
        let f = fixture();
        let alice = make_role_session(&f.auth_path, "alice", &["user"]);

        let (s, body) = capture_locator(&f, &alice, "file:///etc/hosts").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "file:// locators must reference a staged upload");

        // Path traversal out of the staging dir and nonexistent files are rejected too.
        let paths = archivr_core::archive::read_archive_paths(&f.archive_path).unwrap();
        let staging = paths.store_path.join("temp").join("uploads");
        std::fs::create_dir_all(&staging).unwrap();
        let (s, _) = capture_locator(
            &f,
            &alice,
            &format!("file://{}/../../../../../../../../etc/hosts", staging.display()),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = capture_locator(
            &f,
            &alice,
            &format!("file://{}/missing/none.txt", staging.display()),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        // A real file just outside the staging dir.
        let outside = paths.store_path.join("temp").join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let (s, _) = capture_locator(&f, &alice, &format!("file://{}", outside.display())).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        // A bare path (no file:// prefix) is classified as a local file by core and must
        // be refused too: absolute, and relative to the server's cwd.
        let (s, body) = capture_locator(&f, &alice, &outside.display().to_string()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("local paths are not accepted"));
        let (s, _) = capture_locator(&f, &alice, "/etc/hosts").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = capture_locator(&f, &alice, "Cargo.toml").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        // Rejected requests must not leave job rows behind.
        {
            let conn = database::open_or_initialize(&f.archive_path).unwrap();
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM capture_jobs", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0);
        }

        // The normal upload -> capture flow still works.
        let mut upload = multipart_upload("hello.txt", "hello staged world");
        upload
            .headers_mut()
            .insert("cookie", alice.parse().unwrap());
        let resp = f.router.clone().oneshot(upload).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let locator = body_json(resp).await["locator"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(locator.starts_with("file://"));
        let (s, body) = capture_locator(&f, &alice, &locator).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{body}");
        let job_uid = body["job_uid"].as_str().unwrap();
        assert_eq!(wait_job(&f.archive_path, job_uid).await, "completed");
    }

    #[tokio::test]
    async fn list_archives_redacts_archive_path_for_non_admins() {
        let f = fixture();
        let expected = f.archive_path.to_string_lossy().to_string();

        let (s, anon) = get(&f.router, "/api/archives", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(anon, json!([{"id": "test", "label": "Test"}]));

        for (name, roles) in [("guesty", vec!["guest"]), ("usery", vec!["user"])] {
            let cookie = make_role_session(&f.auth_path, name, &roles);
            let (_, list) = get(&f.router, "/api/archives", Some(&cookie)).await;
            assert_eq!(list, json!([{"id": "test", "label": "Test"}]), "{name}");
        }

        let admin = admin_session(&f.auth_path);
        let (_, list) = get(&f.router, "/api/archives", Some(&admin)).await;
        assert_eq!(list[0]["id"], "test");
        assert_eq!(list[0]["archive_path"], expected.as_str());
        let owner = make_test_session(&f.auth_path);
        let (_, list) = get(&f.router, "/api/archives", Some(&owner)).await;
        assert_eq!(list[0]["archive_path"], expected.as_str());

        // Bearer admin tokens count too.
        let token = make_api_token(&f.auth_path, "test-admin", None, "full");
        let resp = f
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/archives")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(resp).await[0]["archive_path"], expected.as_str());
    }
}
