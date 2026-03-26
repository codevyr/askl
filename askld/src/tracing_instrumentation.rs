use diesel::connection::{InstrumentationEvent, Instrumentation};

pub struct TracingInstrumentation {
    query_span: Option<tracing::Span>,
}

impl TracingInstrumentation {
    pub fn new() -> Self {
        Self { query_span: None }
    }
}

impl Instrumentation for TracingInstrumentation {
    fn on_connection_event(&mut self, event: InstrumentationEvent<'_>) {
        match event {
            InstrumentationEvent::StartQuery { query, .. } => {
                let sql = format!("{}", query);
                self.query_span = Some(tracing::info_span!("sql", sql = %sql));
            }
            InstrumentationEvent::FinishQuery { .. } => {
                self.query_span = None;
            }
            _ => {}
        }
    }
}
