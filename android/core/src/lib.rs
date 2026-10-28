//! JNI exports for `app.oasis.mobile.Native`. Every call comes from the app's main
//! thread with the handle `start` returned, so the session needs no lock.

mod frame;
mod grid;
mod session;

use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jbyteArray, jint, jintArray, jlong, jstring};
use jni::JNIEnv;

use session::Session;

static LAST_ERROR: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_lastError(mut env: JNIEnv, _class: JClass) -> jstring {
    let error = LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()).clone();
    new_string(&mut env, &error)
}

fn string(env: &mut JNIEnv, value: &JString) -> String {
    env.get_string(value).map(Into::into).unwrap_or_default()
}

fn new_string(env: &mut JNIEnv, value: &str) -> jstring {
    env.new_string(value)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// `handle` is a pointer `start` returned and `stop` has not freed.
unsafe fn session<'a>(handle: jlong) -> &'a mut Session {
    &mut *(handle as *mut Session)
}

fn cells(cols: jint, rows: jint) -> (u16, u16) {
    (cols.clamp(1, 2000) as u16, rows.clamp(1, 2000) as u16)
}

/// Sets up the process for the core (HOME, PATH with the app's ssh) and connects.
/// Returns 0 on failure, with the reason in `lastError`.
#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_start(
    mut env: JNIEnv,
    _class: JClass,
    home: JString,
    bin: JString,
    machines: JString,
    cols: jint,
    rows: jint,
    cell_w: jint,
    cell_h: jint,
) -> jlong {
    let home = string(&mut env, &home);
    let bin = string(&mut env, &bin);
    let machines = string(&mut env, &machines);
    // Set before the core starts its threads; nothing else reads the environment yet.
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", format!("{bin}:/system/bin"));
    std::env::set_var("TMPDIR", format!("{home}/tmp"));
    std::env::set_var("SHELL", "/system/bin/sh");
    let _ = std::fs::create_dir_all(format!("{home}/tmp"));
    let (cols, rows) = cells(cols, rows);
    match Session::start(&machines, cols, rows, (cell_w.max(0) as u32, cell_h.max(0) as u32)) {
        Ok(session) => Box::into_raw(Box::new(session)) as jlong,
        Err(error) => {
            *LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()) = error;
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_stop(_env: JNIEnv, _class: JClass, handle: jlong) {
    if handle != 0 {
        drop(unsafe { Box::from_raw(handle as *mut Session) });
    }
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_wakeFd(_env: JNIEnv, _class: JClass, handle: jlong) -> jint {
    unsafe { session(handle) }.wake_fd()
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_drain(mut env: JNIEnv, _class: JClass, handle: jlong) -> jstring {
    let out = unsafe { session(handle) }.drain().to_string();
    new_string(&mut env, &out)
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_frame(env: JNIEnv, _class: JClass, handle: jlong) -> jbyteArray {
    let session = unsafe { session(handle) };
    let bytes = frame::encode(&session.grid, &session.palette);
    env.byte_array_from_slice(&bytes)
        .map(|a| a.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_openPane(mut env: JNIEnv, _class: JClass, handle: jlong, endpoint: JString, pane: JString) {
    let (endpoint, pane) = (string(&mut env, &endpoint), string(&mut env, &pane));
    unsafe { session(handle) }.open_pane(&endpoint, &pane);
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_openSpace(mut env: JNIEnv, _class: JClass, handle: jlong, endpoint: JString, space: JString) {
    let (endpoint, space) = (string(&mut env, &endpoint), string(&mut env, &space));
    unsafe { session(handle) }.open_space(&endpoint, &space);
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_retry(mut env: JNIEnv, _class: JClass, handle: jlong, endpoint: JString) {
    let endpoint = string(&mut env, &endpoint);
    unsafe { session(handle) }.retry(&endpoint);
}

/// Sends an API request; returns an error message, or null when sent.
#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_request(mut env: JNIEnv, _class: JClass, handle: jlong, endpoint: JString, method: JString, params: JString) -> jstring {
    let endpoint = string(&mut env, &endpoint);
    let method = string(&mut env, &method);
    let params = serde_json::from_str(&string(&mut env, &params)).unwrap_or_default();
    match unsafe { session(handle) }.request(&endpoint, &method, params) {
        Ok(()) => std::ptr::null_mut(),
        Err(error) => new_string(&mut env, &error),
    }
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_resize(_env: JNIEnv, _class: JClass, handle: jlong, cols: jint, rows: jint, cell_w: jint, cell_h: jint) {
    let (cols, rows) = cells(cols, rows);
    unsafe { session(handle) }.resize(cols, rows, (cell_w.max(0) as u32, cell_h.max(0) as u32));
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_setFocus(_env: JNIEnv, _class: JClass, handle: jlong, focused: jboolean) {
    unsafe { session(handle) }.set_focus(focused != 0);
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_text(mut env: JNIEnv, _class: JClass, handle: jlong, text: JString) {
    let text = string(&mut env, &text);
    unsafe { session(handle) }.text(&text);
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_paste(mut env: JNIEnv, _class: JClass, handle: jlong, text: JString) {
    let text = string(&mut env, &text);
    unsafe { session(handle) }.paste(&text);
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_key(_env: JNIEnv, _class: JClass, handle: jlong, code: jint, modifiers: jint) {
    unsafe { session(handle) }.key(code, modifiers as u8);
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_scroll(_env: JNIEnv, _class: JClass, handle: jlong, lines: jint, col: jint, row: jint) {
    let (col, row) = cells(col + 1, row + 1);
    unsafe { session(handle) }.scroll(lines, col - 1, row - 1);
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_tap(_env: JNIEnv, _class: JClass, handle: jlong, col: jint, row: jint) {
    let (col, row) = cells(col + 1, row + 1);
    unsafe { session(handle) }.tap(col - 1, row - 1);
}

/// The word under a cell, inside its pane: `[col0, row0, col1, row1, paneLeft, paneRight]`.
#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_wordAt(env: JNIEnv, _class: JClass, handle: jlong, col: jint, row: jint) -> jintArray {
    let grid = &unsafe { session(handle) }.grid;
    let at = (col.max(0) as u16, row.max(0) as u16);
    let bounds = grid.pane_columns(at.0, at.1);
    let (from, to) = grid.word_at(at, bounds);
    let values = [from.0, from.1, to.0, to.1, bounds.0, bounds.1].map(i32::from);
    let Ok(array) = env.new_int_array(6) else {
        return std::ptr::null_mut();
    };
    let _ = env.set_int_array_region(&array, 0, &values);
    array.into_raw()
}

#[no_mangle]
pub extern "system" fn Java_app_oasis_mobile_Native_selectionText(mut env: JNIEnv, _class: JClass, handle: jlong, col0: jint, row0: jint, col1: jint, row1: jint, left: jint, right: jint) -> jstring {
    let grid = &unsafe { session(handle) }.grid;
    let u = |v: jint| v.max(0) as u16;
    let text = grid.text((u(col0), u(row0)), (u(col1), u(row1)), (u(left), u(right)));
    new_string(&mut env, &text)
}
