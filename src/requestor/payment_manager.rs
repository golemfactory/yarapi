#![allow(dead_code)]
/* source code from gwasm-runner */
use actix::prelude::*;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::time::Duration;
use ya_client::{model, payment::PaymentApi};

pub struct PaymentManager {
    payment_api: PaymentApi,
    allocation_id: String,
    total_amount: BigDecimal,
    amount_paid: BigDecimal,
    valid_agreements: HashSet<String>,
    last_debit_note_event: DateTime<Utc>,
    last_invoice_event: DateTime<Utc>,
}

impl Actor for PaymentManager {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.update_debit_notes(ctx);
        self.update_invoices(ctx);
    }
}

impl PaymentManager {
    pub fn new(payment_api: PaymentApi, allocation: model::payment::Allocation) -> Self {
        let now = Utc::now();
        PaymentManager {
            payment_api,
            allocation_id: allocation.allocation_id,
            total_amount: allocation.total_amount,
            amount_paid: 0.into(),
            valid_agreements: Default::default(),
            last_debit_note_event: now,
            last_invoice_event: now,
        }
    }

    fn update_debit_notes(&mut self, ctx: &mut <PaymentManager as Actor>::Context) {
        let mut ts = self.last_debit_note_event;
        let api = self.payment_api.clone();

        let f = async move {
            let events = api
                .get_debit_note_events(Some(&ts), Some(Duration::from_secs(60)), Some(5), None)
                .await?;
            for event in events {
                log::debug!("got debit note: {:?}", event);
                ts = event.event_date;
            }
            Ok::<_, anyhow::Error>(ts)
        }
        .into_actor(self)
        .then(|ts, this, ctx: &mut Context<Self>| {
            match ts {
                Ok(ts) => this.last_debit_note_event = ts,
                Err(e) => {
                    log::error!("debit note event error: {}", e);
                }
            }
            ctx.run_later(Duration::from_secs(10), |this, ctx| {
                this.update_debit_notes(ctx)
            });
            fut::ready(())
        });

        let _ = ctx.spawn(f);
    }

    fn update_invoices(&mut self, ctx: &mut <PaymentManager as Actor>::Context) {
        let mut ts = self.last_invoice_event;
        let api = self.payment_api.clone();

        let f = async move {
            let events = api
                .get_invoice_events(Some(&ts), Some(Duration::from_secs(60)), Some(5), None)
                .await?;
            let mut new_invoices = Vec::new();
            for event in events {
                log::debug!("Got invoice: {:?}", event);
                if event.event_type == model::payment::InvoiceEventType::InvoiceReceivedEvent {
                    let invoice = api.get_invoice(&event.invoice_id).await?;
                    new_invoices.push(invoice);
                }
                ts = event.event_date;
            }
            Ok::<_, anyhow::Error>((ts, new_invoices))
        }
        .into_actor(self)
        .then(
            |result: Result<(_, Vec<model::payment::Invoice>), _>,
             this,
             ctx: &mut Context<Self>| {
                match result {
                    Ok((ts, invoices)) => {
                        this.last_invoice_event = ts;
                        for invoice in invoices {
                            let api = this.payment_api.clone();

                            if this.valid_agreements.remove(&invoice.agreement_id) {
                                let invoice_id = invoice.invoice_id;
                                log::info!(
                                    "Accepting invoice amounted {} GLM, issuer: {}",
                                    invoice.amount,
                                    invoice.issuer_id
                                );
                                this.amount_paid += invoice.amount.clone();
                                let acceptance = model::payment::Acceptance {
                                    total_amount_accepted: invoice.amount.clone(),
                                    allocation_id: this.allocation_id.clone(),
                                };
                                let _ = Arbiter::spawn(async move {
                                    if let Err(e) =
                                        api.accept_invoice(&invoice_id, &acceptance).await
                                    {
                                        log::error!("invoice {} accept error: {}", invoice_id, e)
                                    }
                                });
                            } else {
                                let invoice_id = invoice.invoice_id;

                                let spec = model::payment::Rejection {
                                    rejection_reason:
                                        model::payment::RejectionReason::UnsolicitedService,
                                    total_amount_accepted: 0.into(),
                                    message: Some("invoice received before results".to_string()),
                                };
                                let _ = Arbiter::spawn(async move {
                                    if let Err(e) = api.reject_invoice(&invoice_id, &spec).await {
                                        log::error!("invoice: {} reject error: {}", invoice_id, e);
                                    }
                                });
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("invoice processing error: {}", e);
                    }
                }
                ctx.run_later(Duration::from_secs(10), |this, ctx| {
                    this.update_invoices(ctx)
                });
                fut::ready(())
            },
        );

        let _ = ctx.spawn(f);
    }
}

pub struct AcceptAgreement {
    pub agreement_id: String,
}

impl Message for AcceptAgreement {
    type Result = anyhow::Result<()>;
}

impl Handler<AcceptAgreement> for PaymentManager {
    type Result = anyhow::Result<()>;

    fn handle(&mut self, msg: AcceptAgreement, _ctx: &mut Self::Context) -> Self::Result {
        self.valid_agreements.insert(msg.agreement_id);
        Ok(())
    }
}

pub struct GetPending;

impl Message for GetPending {
    type Result = usize;
}

impl Handler<GetPending> for PaymentManager {
    type Result = MessageResult<GetPending>;

    fn handle(&mut self, _msg: GetPending, _ctx: &mut Self::Context) -> Self::Result {
        MessageResult(self.valid_agreements.len())
    }
}

pub(crate) struct ReleaseAllocation;

impl Message for ReleaseAllocation {
    type Result = anyhow::Result<()>;
}

impl Handler<ReleaseAllocation> for PaymentManager {
    type Result = ResponseActFuture<Self, anyhow::Result<()>>;

    fn handle(&mut self, _: ReleaseAllocation, _: &mut Self::Context) -> Self::Result {
        let payment_api = self.payment_api.clone();
        let allocation_id = self.allocation_id.clone();
        Box::new(
            async move {
                payment_api
                    .release_allocation(&allocation_id)
                    .await
                    .map_err(anyhow::Error::from)
            }
            .into_actor(self),
        )
    }
}
