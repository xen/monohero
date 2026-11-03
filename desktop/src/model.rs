use serde_json::{Value, json};
#[derive(Default)]
pub struct Model {
    pub snapshot: Value,
    pub selected: Option<String>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub confirmation: Option<(String, Value)>,
    pub inspected: bool,
}
impl Model {
    pub fn new() -> Self {
        Self {
            snapshot: json!({"queues":[],"jobs":[]}),
            ..Default::default()
        }
    }
    pub fn accept(&mut self, result: Result<Value, String>) {
        match result {
            Ok(data) => {
                if !data["queues"].is_array() || !data["jobs"].is_array() {
                    self.error =
                        Some("Invalid management snapshot; previous data retained.".into());
                    return;
                }
                self.snapshot = data;
                if self.queue().is_none() {
                    self.selected = self.snapshot["queues"]
                        .as_array()
                        .and_then(|queues| queues.first())
                        .and_then(|queue| queue["name"].as_str())
                        .map(str::to_owned);
                }
            }
            Err(error) => self.error = Some(error),
        }
    }
    pub fn queues(&self) -> Vec<Value> {
        self.snapshot["queues"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
    pub fn queue(&self) -> Option<&Value> {
        self.snapshot["queues"]
            .as_array()?
            .iter()
            .find(|queue| queue["name"].as_str() == self.selected.as_deref())
    }
    pub fn jobs(&self) -> Vec<Value> {
        self.snapshot["jobs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|job| job["queue"].as_str() == self.selected.as_deref())
            .cloned()
            .collect()
    }
    pub fn queued_order(&self) -> Vec<i64> {
        self.jobs()
            .iter()
            .filter(|job| job["state"] == "queued")
            .filter_map(|job| job["id"].as_i64())
            .collect()
    }
    pub fn confirm(&mut self, title: String, action: Value) {
        self.confirmation = Some((title, action));
        self.inspected = false;
    }
    pub fn confirmed_action(&mut self) -> Option<Value> {
        let (_, mut action) = self.confirmation.take()?;
        action["acknowledge_failures"] = json!(self.inspected);
        Some(action)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_selects_existing_queue_and_never_leaves_a_phantom_selection() {
        let mut model = Model::new();
        model.selected = Some("removed".into());
        model.accept(Ok(json!({"queues":[{"name":"remaining"}],"jobs":[]})));
        assert_eq!(model.selected.as_deref(), Some("remaining"));
        model.accept(Ok(json!({"queues":[],"jobs":[]})));
        assert!(model.selected.is_none());
    }
    #[test]
    fn snapshot_refresh_does_not_dismiss_action_errors_or_include_other_queues_in_reorder() {
        let mut model = Model::new();
        model.error = Some("Active work cannot be cancelled blindly".into());
        model.selected = Some("p".into());
        model.accept(Ok(json!({"queues":[{"name":"p"},{"name":"q"}],"jobs":[{"id":3,"queue":"p","state":"queued"},{"id":1,"queue":"p","state":"running"},{"id":2,"queue":"q","state":"queued"},{"id":4,"queue":"p","state":"queued"}]})));
        assert!(model.error.is_some());
        assert_eq!(model.queued_order(), vec![3, 4]);
    }
    #[test]
    fn inspection_is_explicit_for_each_destructive_confirmation() {
        let mut model = Model::new();
        model.inspected = true;
        model.confirm("Clear?".into(), json!({"type":"clear","queue":"p"}));
        assert_eq!(
            model.confirmed_action().unwrap()["acknowledge_failures"],
            false
        );
        assert!(model.confirmed_action().is_none());
    }
}
