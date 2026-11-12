//! CLI spelling includes recovery without adding an action to capture/hotkeys.
use super::*;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Action {
    Start,
    Stop,
    Toggle,
    Cancel,
    Retry,
}

fn command(
    action: Action,
    print: bool,
    raw: bool,
) -> Result<ControlCommand, crate::control::ControlError> {
    let action = match action {
        Action::Start => CaptureAction::Start,
        Action::Stop => CaptureAction::Stop,
        Action::Toggle => CaptureAction::Toggle,
        Action::Cancel => CaptureAction::Cancel,
        Action::Retry if !print && !raw => return Ok(ControlCommand::DictationRetry),
        Action::Retry => {
            return Err(crate::control::ControlError::new(
                "invalid-value",
                "retry takes neither --print nor --raw",
            ));
        }
    };
    crate::control::check_flags(action, print, raw)?;
    Ok(ControlCommand::Dictation { action, print, raw })
}

pub(super) fn run(action: Action, print: bool, raw: bool, output: Output) -> u8 {
    match command(action, print, raw) {
        Ok(command) => send(command, output),
        Err(error) => {
            eprintln!("superhex: {}", error.message);
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_has_its_own_wire_command_and_takes_no_capture_flags() {
        let request = ControlRequest::new(command(Action::Retry, false, false).unwrap());
        assert_eq!(request.command, ControlCommand::DictationRetry);
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"version": CONTROL_VERSION, "command": "dictation-retry"})
        );
        assert!(command(Action::Retry, true, false).is_err());
        assert!(command(Action::Retry, false, true).is_err());
        assert!(matches!(
            command(Action::Start, false, true).unwrap(),
            ControlCommand::Dictation {
                action: CaptureAction::Start,
                raw: true,
                ..
            }
        ));
    }
}
