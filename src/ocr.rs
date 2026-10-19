// Copyright (c) 2026 Loupe OCR contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! OCR backend based on `pure-onnx-ocr` (PP-OCRv6 tiny, pure Rust, no C/C++).
//!
//! Design notes (see realtime.md):
//! - One process-wide [`OcrEngine`] is shared behind a [`Mutex`]. It is only
//!   held during the blocking inference call (~0.2-0.4s), so the UI thread is
//!   never blocked and multiple windows run their tasks independently.
//! - Each window owns a monotonically increasing *generation* counter. A
//!   worker thread checks the generation before reporting back; results from
//!   stale generations (image switched or window closed) are dropped.
//! - Tract inference itself cannot be preempted mid-call, so cancellation is
//!   cooperative: the result is discarded and no UI work happens afterwards.
//! - Any OCR failure is converted to [`OcrStatus::Error`] and only touches the
//!   status button. The normal image pipeline is never affected.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::deps::*;

/// One recognized text line in original image pixel coordinates.
#[derive(Debug, Clone)]
pub struct OcrLine {
    pub text: String,
    pub confidence: f32,
    /// Bounding quad: top-left, top-right, bottom-right, bottom-left.
    pub points: [(f64, f64); 4],
}

/// Per-window OCR state machine.
///
/// - `Idle`: nothing to do (e.g. no local file).
/// - `Running`: background worker active (yellow lamp).
/// - `Done`: text available, `lines` is the recognized line count (green lamp).
/// - `Error`: last run failed; `message` is shown in the lamp tooltip (red lamp).
#[derive(Debug, Clone, Default)]
pub enum OcrStatus {
    #[default]
    Idle,
    Running,
    Done { lines: usize },
    Error { message: String },
}

/// Locate the directory holding `tiny_det/` and `tiny_rec/` model dirs.
///
/// Search order:
/// 1. `$LOUPE_OCR_MODELS` (lets developers point at a checkout)
/// 2. `$XDG_DATA_HOME/loupe/ocr-models` (i.e. `~/.local/share/...`)
/// 3. `/usr/local/share/loupe/ocr-models`
/// 4. `/usr/share/loupe/ocr-models`
pub fn model_base_dir() -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(dir) = std::env::var("LOUPE_OCR_MODELS") {
        candidates.push(PathBuf::from(dir));
    }
    candidates.push(glib::user_data_dir().join("loupe/ocr-models"));
    candidates.push(PathBuf::from("/usr/local/share/loupe/ocr-models"));
    candidates.push(PathBuf::from("/usr/share/loupe/ocr-models"));

    for base in &candidates {
        let det = base.join("tiny_det/inference.onnx");
        let rec = base.join("tiny_rec/inference.onnx");
        if det.is_file() && rec.is_file() {
            return Ok(base.clone());
        }
    }

    Err(format!(
        "OCR models not found. Searched: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn build_engine() -> Result<pure_onnx_ocr::OcrEngine, String> {
    let base = model_base_dir()?;
    pure_onnx_ocr::OcrEngineBuilder::new()
        .det_model_dir(base.join("tiny_det"))
        .rec_model_dir(base.join("tiny_rec"))
        .build()
        .map_err(|e| format!("Failed to load OCR models from {}: {e}", base.display()))
}

type EngineGuard = std::sync::MutexGuard<'static, pure_onnx_ocr::OcrEngine>;

fn global_engine() -> Result<EngineGuard, String> {
    static ENGINE: OnceLock<Mutex<pure_onnx_ocr::OcrEngine>> = OnceLock::new();
    static ENGINE_ERROR: OnceLock<String> = OnceLock::new();

    if let Some(engine) = ENGINE.get() {
        return engine
            .lock()
            .map_err(|_| "OCR engine lock poisoned".to_string());
    }
    if let Some(err) = ENGINE_ERROR.get() {
        return Err(err.clone());
    }

    // First caller builds the engine. A rare race may build it twice, but
    // `set` keeps exactly one and the extra is dropped — harmless.
    match build_engine() {
        Ok(engine) => {
            let _ = ENGINE.set(Mutex::new(engine));
            ENGINE
                .get()
                .expect("engine just stored")
                .lock()
                .map_err(|_| "OCR engine lock poisoned".to_string())
        }
        Err(err) => {
            let _ = ENGINE_ERROR.set(err.clone());
            Err(err)
        }
    }
}

/// Message sent by an OCR worker thread back to the main thread.
///
/// `generation` must match the window's current generation, otherwise the
/// result is stale (image switched or window closed) and gets dropped.
#[derive(Debug)]
pub struct OcrEvent {
    pub generation: u64,
    pub outcome: Result<Vec<OcrLine>, String>,
}

/// Run OCR on a local image file (blocking, ~0.2-0.4s on CPU).
///
/// Safe to call from a worker thread. Never touches GTK.
pub fn run_ocr(path: &Path) -> Result<Vec<OcrLine>, String> {
    let engine = global_engine()?;
    let results = engine
        .run_from_path(path)
        .map_err(|e| format!("OCR failed for {}: {e}", path.display()))?;
    drop(engine);

    Ok(results
        .into_iter()
        .map(|r| {
            let mut pts = [(0.0, 0.0); 4];
            for (i, p) in r.bounding_box.exterior().points().take(4).enumerate() {
                pts[i] = (p.x(), p.y());
            }
            OcrLine {
                text: r.text,
                confidence: r.confidence,
                points: pts,
            }
        })
        .collect())
}
