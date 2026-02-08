use std::process::{Child, Command, Output};

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
pub trait CommandRunner: Send + Sync {
    fn output(&self, _cmd: &mut Command) -> Option<std::io::Result<Output>> {
        None
    }

    fn spawn(&self, _cmd: &mut Command) -> Option<std::io::Result<Child>> {
        None
    }
}

#[cfg(test)]
thread_local! {
    static RUNNER_STACK: RefCell<Vec<Arc<dyn CommandRunner>>> = RefCell::new(Vec::new());
}

#[cfg(test)]
fn with_runner<T>(f: impl FnOnce(&dyn CommandRunner) -> Option<T>) -> Option<T> {
    RUNNER_STACK.with(|stack| {
        let stack = stack.borrow();
        let runner = stack.last()?;
        f(runner.as_ref())
    })
}

#[cfg(test)]
pub struct ScopedCommandRunner {
    _private: (),
}

#[cfg(test)]
impl Drop for ScopedCommandRunner {
    fn drop(&mut self) {
        RUNNER_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            let _ = stack.pop();
        });
    }
}

#[cfg(test)]
pub fn scoped_command_runner(runner: Arc<dyn CommandRunner>) -> ScopedCommandRunner {
    RUNNER_STACK.with(|stack| stack.borrow_mut().push(runner));
    ScopedCommandRunner { _private: () }
}

pub fn output(cmd: &mut Command) -> std::io::Result<Output> {
    #[cfg(test)]
    if let Some(result) = with_runner(|runner| runner.output(cmd)) {
        return result;
    }

    cmd.output()
}

pub fn spawn(cmd: &mut Command) -> std::io::Result<Child> {
    #[cfg(test)]
    if let Some(result) = with_runner(|runner| runner.spawn(cmd)) {
        return result;
    }

    cmd.spawn()
}
