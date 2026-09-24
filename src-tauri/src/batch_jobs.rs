//! A batch is an aggregate of individually scheduled file jobs, not one long worker lease.
use crate::{AppState, Shared, runtime, scheduled_job, work};
use domain::{Error, ErrorInfo, Result, TaskOptions, TaskState};
use recognition::ocr::OcrRun;
use std::path::PathBuf;
use task_engine::batch::BatchView;
use tauri::{Emitter, Manager};
use uuid::Uuid;

pub async fn analyze(
    state: Shared<'_>,
    app: tauri::AppHandle,
    id: Uuid,
    run: OcrRun,
) -> Result<BatchView> {
    let view = work(state.clone(), move |engine| engine.batch_view(id)).await?;
    let options = work(state.clone(), |engine| {
        let settings = engine.store.settings()?;
        Ok(TaskOptions {
            ocr_profile: settings.ocr_profile,
            pdf_mode: settings.pdf_mode,
        })
    })
    .await?;
    let total = view.items.len();
    let mut completed = view
        .items
        .iter()
        .filter(|item| item.state != TaskState::Queued)
        .count();
    let mut jobs = tokio::task::JoinSet::new();
    for item in view
        .items
        .into_iter()
        .filter(|item| item.state == TaskState::Queued)
    {
        let app = app.clone();
        let run = run.clone();
        let options = options.clone();
        jobs.spawn(async move {
            let index = item.index;
            let state = app.state::<AppState>();
            // A retry uses its saved settings even if the application defaults changed.
            let options = work(state.clone(), move |engine| {
                Ok(item
                    .task_id
                    .and_then(|task_id| engine.view(task_id).ok())
                    .map(|view| view.options)
                    .unwrap_or(options))
            })
            .await;
            let options = match options {
                Ok(options) => options,
                Err(error) => return (index, Some(error)),
            };
            let requirements = runtime::Requirements::for_file(
                &PathBuf::from(format!("source.{}", item.extension)),
                &options,
            );
            let outcome = scheduled_job(
                state.clone(),
                Some(app.clone()),
                requirements,
                Some(run.clone()),
                move |engine| engine.analyze_batch_item(id, index, run),
            )
            .await;
            (index, outcome.err())
        });
    }
    let mut failure = None;
    while let Some(joined) = jobs.join_next().await {
        match joined {
            Ok((index, error)) => {
                let error = error.map(|error| ErrorInfo::for_error(&error, "文件分析失败"));
                if let Err(error) = work(state.clone(), move |engine| {
                    engine.record_batch_item(id, index, error)
                })
                .await
                {
                    failure = Some(error);
                }
            }
            Err(error) => failure = Some(Error::Io(format!("批次工作线程异常：{error}"))),
        }
        completed += 1;
        let _ = app.emit(
            "batch-progress",
            serde_json::json!({"id":id,"done":completed,"total":total,"stage":"analyzing"}),
        );
    }
    let cancelled = run.is_cancelled();
    let view = work(state, move |engine| {
        engine.finish_batch_analysis(id, cancelled)
    })
    .await?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(view)
}

pub async fn execute(
    state: Shared<'_>,
    app: tauri::AppHandle,
    id: Uuid,
    run: OcrRun,
) -> Result<BatchView> {
    let view = work(state.clone(), move |engine| {
        engine.begin_batch_execution(id)
    })
    .await?;
    let total = view.items.len();
    let mut completed = view
        .items
        .iter()
        .filter(|item| item.state != TaskState::AwaitingReview)
        .count();
    let mut jobs = tokio::task::JoinSet::new();
    for item in view
        .items
        .into_iter()
        .filter(|item| item.state == TaskState::AwaitingReview)
    {
        let Some(task_id) = item.task_id else {
            continue;
        };
        let app = app.clone();
        let run = run.clone();
        jobs.spawn(async move {
            let state = app.state::<AppState>();
            let outcome = scheduled_job(
                state.clone(),
                Some(app.clone()),
                runtime::Requirements::render(),
                Some(run.clone()),
                move |engine| engine.execute_as(task_id, run),
            )
            .await;
            (item.index, outcome.err())
        });
    }
    let mut failure = None;
    while let Some(joined) = jobs.join_next().await {
        match joined {
            Ok((index, error)) => {
                let error = error.map(|error| ErrorInfo::for_error(&error, "文件生成失败"));
                if let Err(error) = work(state.clone(), move |engine| {
                    engine.record_batch_item(id, index, error)
                })
                .await
                {
                    failure = Some(error);
                }
            }
            Err(error) => failure = Some(Error::Io(format!("批次工作线程异常：{error}"))),
        }
        completed += 1;
        let _ = app.emit(
            "batch-progress",
            serde_json::json!({"id":id,"done":completed,"total":total,"stage":"processing"}),
        );
    }
    let cancelled = run.is_cancelled();
    let view = work(state, move |engine| {
        engine.finish_batch_execution(id, cancelled)
    })
    .await?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(view)
}
