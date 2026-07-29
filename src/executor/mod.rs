use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

struct ConsolePanel {
    header: String,
    lines: VecDeque<String>,
    incomplete_line: Option<String>,
    height: usize,
    width: usize,
    first_draw: bool,
}

impl ConsolePanel {
    fn new(header: &str, height: usize) -> Self {
        Self {
            header: header.to_string(),
            lines: VecDeque::with_capacity(height),
            incomplete_line: None,
            height,
            width: 74,
            first_draw: true,
        }
    }

    fn draw(&mut self) {
        let mut stdout = io::stdout();
        let _ = write!(stdout, "\x1B[?25l");

        if !self.first_draw {
            let move_up = self.height + 2;
            let _ = write!(stdout, "\r\x1B[{}A", move_up);
        } else {
            let _ = writeln!(stdout, "{}", self.header);
            self.first_draw = false;
        }

        let _ = write!(stdout, "\r\x1B[2K  ┌{}┐\n", "─".repeat(self.width));

        let mut complete_to_draw = self.height;
        if self.incomplete_line.is_some() {
            complete_to_draw -= 1;
        }

        let num_complete = self.lines.len();
        let empty_pads = if num_complete < complete_to_draw {
            complete_to_draw - num_complete
        } else {
            0
        };

        for _ in 0..empty_pads {
            let inner_width = self.width - 2;
            let display_line = format!("{:width$}", "", width = inner_width);
            let _ = write!(stdout, "\r\x1B[2K  │ {} │\n", display_line);
        }

        let start_idx = if num_complete > complete_to_draw {
            num_complete - complete_to_draw
        } else {
            0
        };
        for idx in start_idx..num_complete {
            if let Some(line_content) = self.lines.get(idx) {
                let inner_width = self.width - 2;
                let display_line = if line_content.len() > inner_width {
                    format!("{}...", &line_content[0..inner_width - 3])
                } else {
                    format!("{:width$}", line_content, width = inner_width)
                };
                let _ = write!(stdout, "\r\x1B[2K  │ {} │\n", display_line);
            }
        }

        if let Some(ref line_content) = self.incomplete_line {
            let clean = line_content
                .trim_end_matches(|c| c == '\n' || c == '\r')
                .to_string();
            let inner_width = self.width - 2;
            let display_line = if clean.len() > inner_width {
                format!("{}...", &clean[0..inner_width - 3])
            } else {
                format!("{:width$}", clean, width = inner_width)
            };
            let _ = write!(stdout, "\r\x1B[2K  │ {} │\n", display_line);
        }

        let _ = write!(stdout, "\r\x1B[2K  └{}┘\n", "─".repeat(self.width));
        let _ = write!(stdout, "\x1B[?25h");
        let _ = stdout.flush();
    }

    fn add_line(&mut self, line: &str) {
        self.incomplete_line = None;
        let clean_line = line
            .trim_end_matches(|c| c == '\n' || c == '\r')
            .to_string();
        for sub_line in clean_line.split('\n') {
            let parts: Vec<&str> = sub_line.split('\r').collect();
            if let Some(last_part) = parts.last() {
                if !last_part.is_empty() {
                    self.lines.push_back(last_part.to_string());
                    if self.lines.len() > self.height {
                        self.lines.pop_front();
                    }
                }
            }
        }
        self.draw();
    }

    fn add_line_incomplete(&mut self, line: &str) {
        let clean_line = line
            .trim_end_matches(|c| c == '\n' || c == '\r')
            .to_string();
        let parts: Vec<&str> = clean_line.split(|c| c == '\n' || c == '\r').collect();
        if let Some(last_part) = parts.last() {
            if !last_part.is_empty() {
                self.incomplete_line = Some(last_part.to_string());
            } else {
                self.incomplete_line = None;
            }
        } else {
            self.incomplete_line = None;
        }
        self.draw();
    }

    fn finish(&mut self, success: bool, final_header: &str) {
        let mut stdout = io::stdout();
        let _ = write!(stdout, "\x1B[?25l");

        if success {
            if !self.first_draw {
                let move_up = self.height + 3;
                let _ = write!(stdout, "\r\x1B[{}A", move_up);
                for _ in 0..move_up {
                    let _ = write!(stdout, "\r\x1B[2K\n");
                }
                let _ = write!(stdout, "\r\x1B[{}A", move_up);
            }
            let _ = writeln!(stdout, "{}", final_header);
        } else {
            if !self.first_draw {
                let move_up = self.height + 3;
                let _ = write!(stdout, "\r\x1B[{}A", move_up);
            }
            let _ = write!(stdout, "\r\x1B[2K{}\n", final_header);

            let red_top = format!("  ┌{}┐", "─".repeat(self.width)).red();
            let _ = write!(stdout, "\r\x1B[2K{}\n", red_top);

            let mut all_lines = Vec::new();
            for line in &self.lines {
                all_lines.push(line.clone());
            }
            if let Some(ref inc) = self.incomplete_line {
                all_lines.push(inc.clone());
            }

            let total = all_lines.len();
            let start = if total > self.height {
                total - self.height
            } else {
                0
            };
            let pad = if total < self.height {
                self.height - total
            } else {
                0
            };

            for _ in 0..pad {
                let inner_width = self.width - 2;
                let display_line = format!("{:width$}", "", width = inner_width);
                let _ = write!(
                    stdout,
                    "\r\x1B[2K  {} {} {}\n",
                    "│".red(),
                    display_line.red(),
                    "│".red()
                );
            }

            for idx in start..total {
                let line_content = &all_lines[idx];
                let inner_width = self.width - 2;
                let display_line = if line_content.len() > inner_width {
                    format!("{}...", &line_content[0..inner_width - 3])
                } else {
                    format!("{:width$}", line_content, width = inner_width)
                };
                let line_to_print = format!("  {} {} {}", "│".red(), display_line.red(), "│".red());
                let _ = write!(stdout, "\r\x1B[2K{}\n", line_to_print);
            }

            let red_bottom = format!("  └{}┘", "─".repeat(self.width)).red();
            let _ = write!(stdout, "\r\x1B[2K{}\n", red_bottom);
        }
        let _ = write!(stdout, "\x1B[?25h");
        let _ = stdout.flush();
    }
}

fn command_summary(command: &str) -> String {
    let first_line = command.lines().next().unwrap_or("").trim();
    if first_line.starts_with("if ") || first_line.is_empty() {
        return "Running remote script".to_string();
    }

    let mut parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() > 5 {
        parts.truncate(5);
        format!("{} ...", parts.join(" "))
    } else {
        parts.join(" ")
    }
}

pub struct RemoteExecutor;

impl RemoteExecutor {
    pub async fn run_interactive(program: &str, args: &[String]) -> Result<()> {
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await
            .with_context(|| format!("Failed to spawn {}", program))?;

        if !status.success() {
            anyhow::bail!("{} exited with error", program);
        }

        Ok(())
    }

    pub async fn ssh_interactive(host: &str, user: &str, command: &str) -> Result<()> {
        let target = format!("{}@{}", user, host);
        let args = vec![
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
            "-tt".to_string(),
            target.clone(),
            command.to_string(),
        ];

        Self::run_interactive("ssh", &args)
            .await
            .with_context(|| format!("Interactive SSH command failed on {}", target))
    }

    pub async fn execute(host: &str, user: &str, command: &str) -> Result<String> {
        let summary = command_summary(command);
        let running_header = format!("      * {} ... {}", summary, "∨".blue());
        let success_header = format!("      * {} ... {}", summary, "OK".green());
        let failure_header = format!("      * {} ... {}", summary, "FAILED".red());

        Self::execute_live(
            host,
            user,
            command,
            &running_header,
            &success_header,
            &failure_header,
        )
        .await
    }

    pub async fn execute_live(
        host: &str,
        user: &str,
        command: &str,
        running_header: &str,
        success_header: &str,
        failure_header: &str,
    ) -> Result<String> {
        let target = format!("{}@{}", user, host);
        let mut child = Command::new("ssh")
            .args(["-o", "ConnectTimeout=5"])
            .args([
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPath=/tmp/devpod-ssh-%r@%h-%p",
                "-o",
                "ControlPersist=60",
            ])
            .arg("-tt")
            .arg(&target)
            .arg(command)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(format!("Failed to spawn command on {}", target))?;

        let mut stdout_reader = child.stdout.take().unwrap();
        let mut stderr_reader = child.stderr.take().unwrap();

        let mut panel = ConsolePanel::new(running_header, 5);
        panel.draw();

        let mut accumulated_stdout = String::new();
        let mut accumulated_stderr = String::new();

        let mut stdout_pending = String::new();
        let mut stderr_pending = String::new();

        let mut stdout_done = false;
        let mut stderr_done = false;

        let mut stdout_buf = [0u8; 1024];
        let mut stderr_buf = [0u8; 1024];

        loop {
            if stdout_done && stderr_done {
                break;
            }

            tokio::select! {
                res = tokio::io::AsyncReadExt::read(&mut stdout_reader, &mut stdout_buf), if !stdout_done => {
                    match res {
                        Ok(0) => {
                            stdout_done = true;
                            if !stdout_pending.is_empty() {
                                panel.add_line(&stdout_pending);
                                stdout_pending.clear();
                            }
                        }
                        Ok(n) => {
                            let s = String::from_utf8_lossy(&stdout_buf[..n]);
                            accumulated_stdout.push_str(&s);
                            stdout_pending.push_str(&s);

                            while let Some(pos) = stdout_pending.find('\n') {
                                let line = stdout_pending[..pos].to_string();
                                panel.add_line(&line);
                                stdout_pending = stdout_pending[pos + 1..].to_string();
                            }
                            if !stdout_pending.is_empty() {
                                panel.add_line_incomplete(&stdout_pending);
                            }
                        }
                        Err(_) => {
                            stdout_done = true;
                        }
                    }
                }
                res = tokio::io::AsyncReadExt::read(&mut stderr_reader, &mut stderr_buf), if !stderr_done => {
                    match res {
                        Ok(0) => {
                            stderr_done = true;
                            if !stderr_pending.is_empty() {
                                panel.add_line(&stderr_pending);
                                stderr_pending.clear();
                            }
                        }
                        Ok(n) => {
                            let s = String::from_utf8_lossy(&stderr_buf[..n]);
                            accumulated_stderr.push_str(&s);
                            stderr_pending.push_str(&s);

                            while let Some(pos) = stderr_pending.find('\n') {
                                let line = stderr_pending[..pos].to_string();
                                panel.add_line(&line);
                                stderr_pending = stderr_pending[pos + 1..].to_string();
                            }
                            if !stderr_pending.is_empty() {
                                panel.add_line_incomplete(&stderr_pending);
                            }
                        }
                        Err(_) => {
                            stderr_done = true;
                        }
                    }
                }
            }
        }

        let status = child.wait().await?;

        if !status.success() {
            panel.finish(false, failure_header);
            let err_msg = if accumulated_stderr.trim().is_empty() {
                accumulated_stdout
            } else {
                accumulated_stderr
            };
            anyhow::bail!("Remote command failed: {}", err_msg.trim());
        }

        panel.finish(true, success_header);
        let stdout = accumulated_stdout.replace("\r", "");
        Ok(stdout)
    }

    pub async fn can_connect(host: &str, user: &str) -> bool {
        let target = format!("{}@{}", user, host);
        for attempt in 0..5 {
            let result = Command::new("ssh")
                .args([
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=5",
                    "-o",
                    "StrictHostKeyChecking=accept-new",
                    "-o",
                    "ControlMaster=auto",
                    "-o",
                    "ControlPath=/tmp/devpod-ssh-%r@%h-%p",
                    "-o",
                    "ControlPersist=60",
                ])
                .arg(&target)
                .arg("true")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;

            if let Ok(status) = result {
                if status.success() {
                    return true;
                }
            }

            if attempt < 4 {
                sleep(Duration::from_secs(2)).await;
            }
        }

        false
    }

    pub async fn first_reachable<'a, I>(hosts: I, user: &str) -> Option<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        for host in hosts {
            if Self::can_connect(host, user).await {
                return Some(host.to_string());
            }
        }

        None
    }

    // For interactive shell
    pub async fn shell(host: &str, user: &str) -> Result<()> {
        let target = format!("{}@{}", user, host);
        let status = Command::new("ssh")
            .args(["-o", "ConnectTimeout=5"])
            .args([
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPath=/tmp/devpod-ssh-%r@%h-%p",
                "-o",
                "ControlPersist=60",
            ])
            .arg(&target)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;

        if !status.success() {
            anyhow::bail!("SSH session exited with error");
        }
        Ok(())
    }

    pub async fn scp_from(
        host: &str,
        user: &str,
        remote_path: &str,
        local_path: &str,
    ) -> Result<()> {
        let source = format!("{}@{}:{}", user, host, remote_path);
        let status = Command::new("scp")
            .arg(&source)
            .arg(local_path)
            .status()
            .await
            .context("Failed to SCP file")?;

        if !status.success() {
            anyhow::bail!("SCP failed");
        }
        Ok(())
    }
}
