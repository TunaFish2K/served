//! Post-save restart workflow. Saving and service lifecycle changes stay separate.
use super::view;
use crate::{
    client,
    paths::ServedPaths,
    protocol::{Request, Response, ServiceInfo, ServiceKind, ServiceState, Target},
};
use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{future::Future, io, path::Path, time::Duration};

type Screen = Terminal<CrosstermBackend<io::Stdout>>;

fn active(service: &ServiceInfo) -> bool {
    matches!(
        service.state,
        ServiceState::Running | ServiceState::Starting | ServiceState::Restarting
    )
}

fn matching_service(
    services: Vec<ServiceInfo>,
    file: &Path,
    bound_name: Option<&str>,
) -> Result<Option<ServiceInfo>> {
    let file = std::fs::canonicalize(file).context("Cannot resolve the saved configuration")?;
    let mut matches = services.into_iter().filter(|service| {
        service.kind == ServiceKind::Enabled
            && bound_name.is_none_or(|name| service.name == name)
            && service.config_file.as_ref().is_some_and(|source| {
                std::fs::canonicalize(source).is_ok_and(|source| source == file)
            })
    });
    let result = matches.next();
    if matches.next().is_some() {
        bail!(
            "Several services reference this configuration; restart the intended service by name."
        );
    }
    Ok(result)
}

async fn lookup(
    paths: &ServedPaths,
    file: &Path,
    bound_name: Option<&str>,
) -> Result<Option<ServiceInfo>> {
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        client::request(paths, Request::List),
    )
    .await
    .context("Cannot check service state: manager did not respond within two seconds")?
    .context("Cannot check service state")?;
    match response {
        Response::Services { services } => matching_service(services, file, bound_name),
        Response::Error { message } => bail!("Cannot check service state: {message}"),
        _ => bail!("Cannot check service state: unexpected manager response"),
    }
}

async fn operation<T>(
    terminal: &mut Screen,
    message: &str,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::pin!(future);
    loop {
        terminal.draw(|frame| view::draw_restart(frame, message, None))?;
        // Consume input while pending so held/repeated keys cannot submit twice.
        while event::poll(Duration::ZERO)? {
            let _ = event::read()?;
        }
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

pub(super) async fn after_save(
    terminal: &mut Screen,
    paths: &ServedPaths,
    file: &Path,
    saved_name: &str,
    bound_name: Option<&str>,
) -> Result<()> {
    let service = operation(
        terminal,
        "Checking service state…",
        lookup(paths, file, bound_name),
    )
    .await?;
    let Some(service) = service else {
        return Ok(());
    };
    if service.name != saved_name {
        bail!(
            "The service was renamed. Disable the old service and enable the new configuration before restarting."
        );
    }
    if !active(&service) {
        return Ok(());
    }
    let message = format!("Restart {} to apply changes?", service.name);
    let mut selected = 0;
    loop {
        terminal.draw(|frame| view::draw_restart(frame, &message, Some(selected)))?;
        if !event::poll(Duration::from_millis(50))? {
            tokio::task::yield_now().await;
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if (key.code == KeyCode::Esc && key.modifiers.is_empty())
            || (key.modifiers == KeyModifiers::CONTROL
                && matches!(key.code, KeyCode::Char('c' | 'q')))
        {
            return Ok(());
        }
        let size = terminal.size()?;
        if size.width < 40 || size.height < 10 || !key.modifiers.is_empty() {
            continue;
        }
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => selected = 1 - selected,
            KeyCode::Enter if selected == 0 => return Ok(()),
            KeyCode::Enter => break,
            _ => {}
        }
    }
    operation(terminal, &format!("Restarting {}…", service.name), async {
        let current = lookup(paths, file, Some(&service.name))
            .await?
            .context("Restart unavailable: the service no longer references this configuration")?;
        if !active(&current) {
            bail!("Restart skipped: the service is no longer running. Use Start when ready.");
        }
        let response = tokio::time::timeout(
            Duration::from_secs(10),
            client::request(
                paths,
                Request::Restart {
                    target: Target::Name(service.name.clone()),
                },
            ),
        )
        .await
        .context(
            "Restart did not respond within ten seconds; check the service state before retrying",
        )?
        .context("Restart failed")?;
        match response {
            Response::Ok => Ok(()),
            Response::Error { message } => bail!("Restart failed: {message}"),
            _ => bail!("Restart failed: unexpected manager response"),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service(file: &Path, name: &str) -> ServiceInfo {
        ServiceInfo {
            config_file: Some(file.display().to_string()),
            name: name.into(),
            directory: "/same/workdir".into(),
            kind: ServiceKind::Enabled,
            state: ServiceState::Running,
            pid: Some(1),
            tty: true,
            restart: "never".into(),
            persist_logs: false,
            attach_active: false,
            output_tail: String::new(),
        }
    }
    #[test]
    fn matching_uses_source_identity_not_shared_workdir_or_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.json5");
        let b = dir.path().join("b.json5");
        let alias = dir.path().join("alias");
        std::fs::write(&a, "{}").unwrap();
        std::fs::write(&b, "{}").unwrap();
        std::os::unix::fs::symlink(&a, &alias).unwrap();
        let services = vec![service(&a, "a"), service(&b, "b")];
        assert_eq!(
            matching_service(services.clone(), &alias, None)
                .unwrap()
                .unwrap()
                .name,
            "a"
        );
        assert!(matching_service(services, &a, Some("b")).unwrap().is_none());
        assert!(matching_service(vec![service(&a, "a"), service(&a, "b")], &a, None).is_err());
        let mut temporary = service(&a, "temp");
        temporary.kind = ServiceKind::Temporary;
        assert!(
            matching_service(vec![temporary], &a, None)
                .unwrap()
                .is_none()
        );
        for state in [
            ServiceState::Running,
            ServiceState::Starting,
            ServiceState::Restarting,
        ] {
            let mut running = service(&a, "a");
            running.state = state;
            assert!(active(&running));
        }
        for state in [ServiceState::Stopped, ServiceState::Failed] {
            let mut stopped = service(&a, "a");
            stopped.state = state;
            assert!(!active(&stopped));
        }
    }
    #[tokio::test]
    async fn unresponsive_manager_has_a_bounded_state_check() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ServedPaths::from_home(dir.path());
        std::fs::create_dir_all(paths.socket_path().parent().unwrap()).unwrap();
        let _listener = tokio::net::UnixListener::bind(paths.socket_path()).unwrap();
        let error = lookup(&paths, &dir.path().join("config"), None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("two seconds"));
    }
}
