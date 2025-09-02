/// A unique identifier for an element that can be inspected.
#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct InspectorElementId {}

impl Into<InspectorElementId> for &InspectorElementId {
    fn into(self) -> InspectorElementId {
        self.clone()
    }
}
