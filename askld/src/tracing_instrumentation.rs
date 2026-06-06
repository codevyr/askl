use diesel::connection::{InstrumentationEvent, Instrumentation};

pub struct TracingInstrumentation {
    query_span: Option<tracing::Span>,
}

impl TracingInstrumentation {
    pub fn new() -> Self {
        Self { query_span: None }
    }

    fn exit_span(&mut self) {
        self.query_span.take();
    }
}

impl Instrumentation for TracingInstrumentation {
    fn on_connection_event(&mut self, event: InstrumentationEvent<'_>) {
        match event {
            InstrumentationEvent::StartQuery { query, .. } => {
                self.exit_span();
                let sql = format!("{}", query);
                self.query_span = Some(tracing::debug_span!("sql", sql = %sql));
            }
            InstrumentationEvent::FinishQuery { .. } => {
                self.exit_span();
            }
            _ => {}
        }
    }
}
