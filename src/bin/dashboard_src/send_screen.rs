use std::{
    cmp::max,
    error::Error,
    sync::Arc,
    time::{Duration, SystemTime},
};

use super::{
    dashboard_app::{ConsoleIO, DashboardEvent},
    overview_screen::VerticalRectifier,
    screen::Screen,
};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use neptune_core::{
    config_models::network::Network,
    models::{
        blockchain::transaction::neptune_coins::NeptuneCoins,
        state::wallet::address::generation_address,
    },
    rpc_server::RPCClient,
};

use num_traits::Zero;
use ratatui::{
    layout::{Alignment, Margin},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget},
};
use tarpc::context;
use tokio::{sync::Mutex, time::sleep};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendScreenWidget {
    Address,
    Amount,
    Ok,
    Notice,
}

#[derive(Debug, Clone)]
pub struct SendScreen {
    active: bool,
    fg: Color,
    bg: Color,
    in_focus: bool,
    address: String,
    rpc_client: Arc<RPCClient>,
    focus: Arc<Mutex<SendScreenWidget>>,
    amount: String,
    notice: Arc<Mutex<String>>,
    reset_me: Arc<Mutex<bool>>,
    escalatable_event: Arc<std::sync::Mutex<Option<DashboardEvent>>>,
    network: Network,
}

impl SendScreen {
    pub fn new(rpc_server: Arc<RPCClient>, network: Network) -> Self {
        SendScreen {
            active: false,
            fg: Color::Gray,
            bg: Color::Black,
            in_focus: false,
            address: "".to_string(),
            rpc_client: rpc_server,
            focus: Arc::new(Mutex::new(SendScreenWidget::Address)),
            amount: "".to_string(),
            notice: Arc::new(Mutex::new("".to_string())),
            reset_me: Arc::new(Mutex::new(false)),
            escalatable_event: Arc::new(std::sync::Mutex::new(None)),
            network,
        }
    }

    async fn check_and_pay_sequence(
        rpc_client: Arc<RPCClient>,
        address: String,
        amount: String,
        notice_arc: Arc<Mutex<String>>,
        focus_arc: Arc<Mutex<SendScreenWidget>>,
        reset_me: Arc<Mutex<bool>>,
        network: Network,
    ) {
        *focus_arc.lock().await = SendScreenWidget::Notice;
        *notice_arc.lock().await = "Validating input ...".to_string();
        let maybe_valid_address: Option<generation_address::ReceivingAddress> = rpc_client
            .validate_address(context::current(), address, network)
            .await
            .unwrap();
        let valid_address = match maybe_valid_address {
            Some(add) => add,
            None => {
                *notice_arc.lock().await = "Invalid address.".to_string();
                *focus_arc.lock().await = SendScreenWidget::Address;
                return;
            }
        };

        *notice_arc.lock().await = "Validated address; validating amount ...".to_string();

        let maybe_valid_amount = rpc_client
            .validate_amount(context::current(), amount)
            .await
            .unwrap();
        let valid_amount = match maybe_valid_amount {
            Some(amt) => amt,
            None => {
                *notice_arc.lock().await = "Invalid amount.".to_string();
                *focus_arc.lock().await = SendScreenWidget::Amount;
                return;
            }
        };

        *notice_arc.lock().await = "Validated amount; checking against balance ...".to_string();

        let enough_balance = rpc_client
            .amount_leq_synced_balance(context::current(), valid_amount)
            .await
            .unwrap();
        if !enough_balance {
            *notice_arc.lock().await = "Insufficient balance.".to_string();
            *focus_arc.lock().await = SendScreenWidget::Amount;
            return;
        }

        *notice_arc.lock().await = "Validated inputs; sending ...".to_string();

        // TODO: Let user specify this number
        let fee = NeptuneCoins::zero();

        // Allow the generation of proves to take some time...
        let mut send_ctx = context::current();
        const SEND_DEADLINE_IN_SECONDS: u64 = 40;
        send_ctx.deadline = SystemTime::now() + Duration::from_secs(SEND_DEADLINE_IN_SECONDS);
        let send_result = rpc_client
            .send(send_ctx, valid_amount, valid_address, fee)
            .await
            .unwrap();

        if send_result.is_none() {
            *notice_arc.lock().await = "Could not send due to error.".to_string();
            *focus_arc.lock().await = SendScreenWidget::Address;
            return;
        }

        *notice_arc.lock().await = "Payment broadcast!".to_string();

        sleep(Duration::from_secs(3)).await;

        *notice_arc.lock().await = "".to_string();
        *focus_arc.lock().await = SendScreenWidget::Address;
        *reset_me.lock().await = true;
    }

    pub fn handle(
        &mut self,
        event: DashboardEvent,
    ) -> Result<Option<DashboardEvent>, Box<dyn Error>> {
        if let Ok(mut reset_me_mutex_guard) = self.reset_me.try_lock() {
            if reset_me_mutex_guard.to_owned() {
                self.amount = "".to_string();
                self.address = "".to_string();
                *reset_me_mutex_guard = false;
            }
        }
        let mut escalate_event = None;
        if self.in_focus {
            match event {
                DashboardEvent::ConsoleEvent(Event::Key(key))
                    if key.kind == KeyEventKind::Press =>
                {
                    match key.code {
                        KeyCode::Enter => {
                            if let Ok(mut own_focus) = self.focus.try_lock() {
                                match own_focus.to_owned() {
                                    SendScreenWidget::Address => {
                                        return Ok(Some(DashboardEvent::ConsoleMode(
                                            ConsoleIO::InputRequested(
                                                "Please enter recipient address:\n".to_string(),
                                            ),
                                        )));
                                    }
                                    SendScreenWidget::Amount => {
                                        *own_focus = SendScreenWidget::Ok;
                                        escalate_event = Some(DashboardEvent::RefreshScreen);
                                    }
                                    SendScreenWidget::Ok => {
                                        // clone outside of async section
                                        let rpc_client = self.rpc_client.clone();
                                        let address = self.address.clone();
                                        let amount = self.amount.clone();
                                        let notice = self.notice.clone();
                                        let focus = self.focus.clone();
                                        let reset_me = self.reset_me.clone();
                                        let network = self.network;

                                        tokio::spawn(async move {
                                            Self::check_and_pay_sequence(
                                                rpc_client, address, amount, notice, focus,
                                                reset_me, network,
                                            )
                                            .await;
                                        });
                                        escalate_event = Some(DashboardEvent::RefreshScreen);
                                    }
                                    _ => {
                                        escalate_event = None;
                                    }
                                }
                            }
                        }
                        KeyCode::Up => {
                            if let Ok(mut own_focus) = self.focus.try_lock() {
                                *own_focus = match own_focus.to_owned() {
                                    SendScreenWidget::Address => SendScreenWidget::Ok,
                                    SendScreenWidget::Amount => SendScreenWidget::Address,
                                    SendScreenWidget::Ok => SendScreenWidget::Amount,
                                    SendScreenWidget::Notice => SendScreenWidget::Notice,
                                };
                                escalate_event = Some(DashboardEvent::RefreshScreen);
                            } else {
                                escalate_event = Some(event);
                            }
                        }
                        KeyCode::Down => {
                            if let Ok(mut own_focus) = self.focus.try_lock() {
                                *own_focus = match own_focus.to_owned() {
                                    SendScreenWidget::Address => SendScreenWidget::Amount,
                                    SendScreenWidget::Amount => SendScreenWidget::Ok,
                                    SendScreenWidget::Ok => SendScreenWidget::Address,
                                    SendScreenWidget::Notice => SendScreenWidget::Notice,
                                };
                                escalate_event = Some(DashboardEvent::RefreshScreen);
                            } else {
                                escalate_event = Some(event);
                            }
                        }
                        KeyCode::Char(c) => {
                            if let Ok(own_focus) = self.focus.try_lock() {
                                if own_focus.to_owned() == SendScreenWidget::Amount {
                                    self.amount = format!("{}{}", self.amount, c);
                                    escalate_event = Some(DashboardEvent::RefreshScreen);
                                } else {
                                    escalate_event = Some(event);
                                }
                            } else {
                                escalate_event = Some(event);
                            }
                        }
                        KeyCode::Backspace => {
                            if let Ok(own_focus) = self.focus.try_lock() {
                                if own_focus.to_owned() == SendScreenWidget::Amount {
                                    if !self.amount.is_empty() {
                                        self.amount.drain(self.amount.len() - 1..);
                                    }
                                    escalate_event = Some(DashboardEvent::RefreshScreen);
                                }
                            } else {
                                escalate_event = Some(event);
                            }
                        }
                        _ => {
                            escalate_event = Some(event);
                        }
                    }
                }
                DashboardEvent::ConsoleMode(ConsoleIO::InputSupplied(string)) => {
                    if let Ok(mut own_focus) = self.focus.try_lock() {
                        self.address = string.trim().to_owned();
                        *own_focus = SendScreenWidget::Amount;
                        escalate_event = Some(DashboardEvent::RefreshScreen);
                    } else {
                        escalate_event = Some(DashboardEvent::ConsoleMode(
                            ConsoleIO::InputSupplied(string),
                        ));
                    }
                }
                _ => {
                    escalate_event = None;
                }
            }
        }
        Ok(escalate_event)
    }
}

impl Screen for SendScreen {
    fn activate(&mut self) {
        self.active = true;
    }

    fn deactivate(&mut self) {
        self.active = false;
    }

    fn focus(&mut self) {
        self.fg = Color::White;
        self.in_focus = true;
    }

    fn unfocus(&mut self) {
        self.fg = Color::Gray;
        self.in_focus = false;
    }

    fn escalatable_event(&self) -> Arc<std::sync::Mutex<Option<DashboardEvent>>> {
        self.escalatable_event.clone()
    }
}

impl Widget for SendScreen {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let own_focus = if let Ok(of) = self.focus.try_lock() {
            of.to_owned()
        } else {
            SendScreenWidget::Notice
        };
        // receive box
        let style: Style = if self.in_focus {
            Style::default().fg(Color::LightCyan).bg(self.bg)
        } else {
            Style::default().fg(Color::Gray).bg(self.bg)
        };
        Block::default()
            .borders(Borders::ALL)
            .title("Send")
            .style(style)
            .render(area, buf);

        // divide the overview box vertically into subboxes,
        // and render each separately
        let style = Style::default().bg(self.bg).fg(self.fg);
        let focus_style = Style::default().bg(self.bg).fg(Color::LightCyan);
        let inner = area.inner(&Margin {
            vertical: 1,
            horizontal: 1,
        });
        let width = max(0, inner.width as isize - 2) as usize;
        if width > 0 {
            let mut vrecter = VerticalRectifier::new(inner);

            // display address widget
            let mut address = if let Ok(mg) = self.reset_me.try_lock() {
                if mg.to_owned() {
                    "".to_string()
                } else {
                    self.address.clone()
                }
            } else {
                self.address.clone()
            };
            let mut address_lines = vec![];
            while address.len() > width {
                let (line, remainder) = address.split_at(width);
                address_lines.push(line.to_owned());
                address = remainder.to_owned();
            }
            address_lines.push(address);

            let address_rect = vrecter.next((address_lines.len() + 2).try_into().unwrap());
            if address_rect.height > 0 {
                let address_display = Paragraph::new(Text::from(address_lines.join("\n")))
                    .style(if own_focus == SendScreenWidget::Address && self.in_focus {
                        focus_style
                    } else {
                        style
                    })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(Span::styled("Recipient Address", Style::default())),
                    )
                    .alignment(Alignment::Left);
                address_display.render(address_rect, buf);
            }
            let instruction_rect = vrecter.next(1);
            if instruction_rect.height > 0 {
                let instructions = if self.in_focus && own_focus == SendScreenWidget::Address {
                    Line::from(vec![
                        Span::from("Press "),
                        Span::styled("Enter ↵", Style::default().fg(Color::LightCyan)),
                        Span::from(" to enter address via console mode."),
                    ])
                } else {
                    Line::from(vec![])
                };
                let instructions_widget = Paragraph::new(instructions).style(style);
                instructions_widget.render(instruction_rect, buf);
            }

            // display amount widget
            let amount = if let Ok(mg) = self.reset_me.try_lock() {
                if mg.to_owned() {
                    "".to_string()
                } else {
                    self.amount
                }
            } else {
                self.amount
            };
            let amount_rect = vrecter.next(3);
            let amount_widget = Paragraph::new(Line::from(vec![
                Span::from(amount),
                if own_focus == SendScreenWidget::Amount {
                    Span::styled(
                        "|",
                        if self.in_focus {
                            Style::default().add_modifier(Modifier::RAPID_BLINK)
                        } else {
                            style
                        },
                    )
                } else {
                    Span::from(" ")
                },
            ]))
            .style(if own_focus == SendScreenWidget::Amount && self.in_focus {
                focus_style
            } else {
                style
            })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Amount")
                    .style(if own_focus == SendScreenWidget::Amount && self.in_focus {
                        focus_style
                    } else {
                        style
                    }),
            );
            amount_widget.render(amount_rect, buf);

            // send button
            let mut button_rect = vrecter.next(3);
            button_rect.width = 8;
            let button_widget = Paragraph::new(Span::styled(
                " SEND ",
                if own_focus == SendScreenWidget::Ok && self.in_focus {
                    focus_style
                } else {
                    style
                },
            ))
            .block(Block::default().borders(Borders::ALL).style(
                if own_focus == SendScreenWidget::Ok && self.in_focus {
                    focus_style
                } else {
                    style
                },
            ));
            button_widget.render(button_rect, buf);

            // notice
            if let Ok(notice_text) = self.notice.try_lock() {
                vrecter.next(1);
                let notice_rect = vrecter.next(1);
                let notice_widget = Paragraph::new(Span::styled(
                    notice_text.to_string(),
                    if own_focus == SendScreenWidget::Notice && self.in_focus {
                        focus_style
                    } else {
                        style
                    },
                ));
                notice_widget.render(notice_rect, buf);
            }
        }
    }
}
