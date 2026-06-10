//! Photoproof desktop shell. Contract: spec/UI.md, spec/CAPTURE.md §3–4.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    photoproof_desktop::run();
}
