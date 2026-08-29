use crate::runner::{self, Event as RunnerEvent};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use jio_client::{Event as ClientEvent, VmResult};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Gauge, Paragraph, Row, Table};
use ratatui::{DefaultTerminal, Frame};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

pub fn run(source: PathBuf, host: String, instances: usize) -> io::Result<()> {
    let events = runner::start(source, host.clone(), instances);
    let mut terminal = ratatui::try_init()?;
    let result = App::new(host, instances).run(&mut terminal, events);
    let restore = ratatui::try_restore();
    result?;
    restore
}

struct App {
    host: String,
    phase: String,
    compiled: Option<Duration>,
    artifact_ready: Option<Duration>,
    workload_loaded: Option<Duration>,
    template_loaded: Option<Duration>,
    total: Option<Duration>,
    vms: Vec<Option<VmResult>>,
    error: Option<String>,
    finished: bool,
}

impl App {
    fn new(host: String, instances: usize) -> Self {
        Self {
            host,
            phase: "Starting".into(),
            compiled: None,
            artifact_ready: None,
            workload_loaded: None,
            template_loaded: None,
            total: None,
            vms: vec![None; instances],
            error: None,
            finished: false,
        }
    }

    fn run(
        mut self,
        terminal: &mut DefaultTerminal,
        events: Receiver<RunnerEvent>,
    ) -> io::Result<()> {
        loop {
            loop {
                match events.try_recv() {
                    Ok(event) => self.apply(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if !self.finished {
                            self.fail("runner stopped without a result".into());
                        }
                        break;
                    }
                }
            }

            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    let quit = key.kind == KeyEventKind::Press
                        && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc);
                    if quit && self.finished {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn apply(&mut self, event: RunnerEvent) {
        match event {
            RunnerEvent::Phase(phase) => self.phase = phase,
            RunnerEvent::Compiled(duration) => self.compiled = Some(duration),
            RunnerEvent::Client(event) => self.apply_client(event),
            RunnerEvent::Done(duration) => {
                self.total = Some(duration);
                self.phase = format!("{} VMs completed", self.vms.len());
                self.finished = true;
            }
            RunnerEvent::Error(error) => self.fail(error),
        }
    }

    fn apply_client(&mut self, event: ClientEvent) {
        match event {
            ClientEvent::Phase(phase) => self.phase = phase,
            ClientEvent::ArtifactReady(duration) => self.artifact_ready = Some(duration),
            ClientEvent::WorkloadLoaded(duration) => self.workload_loaded = Some(duration),
            ClientEvent::TemplateLoaded(duration) => self.template_loaded = Some(duration),
            ClientEvent::Vm(result) if (1..=self.vms.len()).contains(&result.index) => {
                let index = result.index - 1;
                self.vms[index] = Some(result);
            }
            ClientEvent::Vm(result) => {
                self.fail(format!("Core returned invalid VM index {}", result.index));
            }
            ClientEvent::Done(_) => {}
        }
    }

    fn fail(&mut self, error: String) {
        self.phase = "Execution failed".into();
        self.error = Some(error);
        self.finished = true;
    }

    fn draw(&self, frame: &mut Frame) {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(9),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());
        self.draw_header(frame, areas[0]);
        self.draw_metrics(frame, areas[1]);
        self.draw_vms(frame, areas[2]);
        self.draw_progress(frame, areas[3]);
        self.draw_footer(frame, areas[4]);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let total = self
            .total
            .map(format_duration)
            .unwrap_or_else(|| "running".into());
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(" JIO ", Style::new().black().on_cyan().bold())),
                Line::from(vec![
                    Span::styled(" host ", Style::new().dark_gray()),
                    Span::raw(&self.host),
                    Span::styled("   elapsed ", Style::new().dark_gray()),
                    Span::raw(total),
                ]),
            ]),
            area,
        );
    }

    fn draw_metrics(&self, frame: &mut Frame, area: Rect) {
        let areas = Layout::horizontal([
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
        ])
        .spacing(1)
        .split(area);
        draw_metric(frame, areas[0], "COMPILE", self.compiled);
        draw_metric(frame, areas[1], "ARTIFACT", self.artifact_ready);
        draw_metric(frame, areas[2], "WORKLOAD LOAD", self.workload_loaded);
        draw_metric(frame, areas[3], "TEMPLATE LOAD", self.template_loaded);
    }

    fn draw_vms(&self, frame: &mut Frame, area: Rect) {
        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(error.as_str())
                    .style(Style::new().red())
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(
                        Block::bordered()
                            .border_type(BorderType::Rounded)
                            .border_style(Style::new().red())
                            .title(" ERROR "),
                    ),
                area,
            );
            return;
        }

        let rows = self.vms.iter().enumerate().map(|(index, result)| {
            if let Some(result) = result {
                Row::new(vec![
                    format!("{:02}", index + 1),
                    "complete".into(),
                    format_duration(result.cow_fork),
                    format_duration(result.restore),
                    format_duration(result.ready),
                    format_duration(result.workload_send),
                    format_duration(result.result_wait),
                    format_duration(result.teardown),
                    result.output.clone(),
                ])
                .style(Style::new().fg(Color::Green))
            } else {
                Row::new(vec![
                    format!("{:02}", index + 1),
                    "waiting".into(),
                    "—".into(),
                    "—".into(),
                    "—".into(),
                    "—".into(),
                    "—".into(),
                    "—".into(),
                    "—".into(),
                ])
                .style(Style::new().fg(Color::DarkGray))
            }
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(12),
                Constraint::Length(11),
                Constraint::Min(24),
            ],
        )
        .header(
            Row::new([
                "VM",
                "STATUS",
                "FORK",
                "RESTORE",
                "READY",
                "SEND",
                "RESULT WAIT",
                "TEARDOWN",
                "OUTPUT",
            ])
            .style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        )
        .column_spacing(1)
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().dark_gray())
                .title(format!(" {} MICROVM ", self.vms.len())),
        );
        frame.render_widget(table, area);
    }

    fn draw_progress(&self, frame: &mut Frame, area: Rect) {
        let completed = self.vms.iter().filter(|result| result.is_some()).count();
        let color = if self.error.is_some() {
            Color::Red
        } else if self.finished {
            Color::Green
        } else {
            Color::Cyan
        };
        frame.render_widget(
            Gauge::default()
                .block(Block::bordered().border_type(BorderType::Rounded))
                .gauge_style(Style::new().fg(color).add_modifier(Modifier::BOLD))
                .ratio(completed as f64 / self.vms.len() as f64)
                .label(format!("{}  •  {completed}/{}", self.phase, self.vms.len())),
            area,
        );
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let help = if self.finished {
            " q / esc  quit"
        } else {
            " restoring VMs from one existing template"
        };
        frame.render_widget(Paragraph::new(help).style(Style::new().dark_gray()), area);
    }
}

fn draw_metric(frame: &mut Frame, area: Rect, title: &str, duration: Option<Duration>) {
    let value = duration.map(format_duration).unwrap_or_else(|| "—".into());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            value,
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        )))
        .centered()
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().dark_gray())
                .title(Span::styled(title, Style::new().fg(Color::DarkGray))),
        ),
        area,
    );
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2} s", duration.as_secs_f64())
    } else if duration.as_millis() > 0 {
        format!("{:.1} ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{} µs", duration.as_micros())
    }
}
