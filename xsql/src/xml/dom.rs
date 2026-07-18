//! Arena-based mutable XML DOM. Nodes live in a flat `Vec` and reference each
//! other by index, which keeps traversal cache-friendly and makes read-only
//! parallel evaluation over `&Document` trivially safe.

pub type NodeId = usize;

/// Reserved tag marking a comment node (`IGNORE_COMMENTS = OFF`): the comment
/// text lives in `text`. `!` can never start a real XML tag name, so this
/// cannot collide with an element.
pub const COMMENT_TAG: &str = "!--";

#[derive(Debug, Clone)]
pub struct Element {
    pub tag: String,
    /// Attributes in document order.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<NodeId>,
    /// Concatenated text content directly inside this element.
    pub text: String,
    pub parent: Option<NodeId>,
}

impl Element {
    pub fn is_comment(&self) -> bool {
        self.tag == COMMENT_TAG
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, name: &str, value: String) {
        match self.attrs.iter_mut().find(|(k, _)| k == name) {
            Some((_, v)) => *v = value,
            None => self.attrs.push((name.to_string(), value)),
        }
    }

    /// Returns `true` if the attribute existed.
    pub fn remove_attr(&mut self, name: &str) -> bool {
        let before = self.attrs.len();
        self.attrs.retain(|(k, _)| k != name);
        self.attrs.len() != before
    }
}

#[derive(Debug, Clone, Default)]
pub struct Document {
    pub nodes: Vec<Element>,
    /// Top-level elements in document order (normally a single root).
    pub roots: Vec<NodeId>,
    pub had_decl: bool,
}

impl Document {
    pub fn node(&self, id: NodeId) -> &Element {
        &self.nodes[id]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Element {
        &mut self.nodes[id]
    }

    pub fn push(&mut self, element: Element) -> NodeId {
        self.nodes.push(element);
        self.nodes.len() - 1
    }

    /// Approximate heap footprint of the DOM arena, in bytes (nodes plus
    /// their strings/attribute/children buffers).
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.nodes.capacity() * std::mem::size_of::<Element>()
            + self.nodes.iter().map(element_bytes).sum::<usize>()
            + self.roots.capacity() * std::mem::size_of::<NodeId>()
    }

    /// Finds a "group": the first element (document order, depth-first) whose
    /// tag equals `name`, or whose `name`/`id` attribute equals `name`.
    pub fn find_group(&self, name: &str) -> Option<NodeId> {
        let mut stack: Vec<NodeId> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            let el = self.node(id);
            if el.tag == name || el.attr("name") == Some(name) || el.attr("id") == Some(name) {
                return Some(id);
            }
            stack.extend(el.children.iter().rev());
        }
        None
    }

    /// Detaches `id` from its parent (or from the root list).
    pub fn detach(&mut self, id: NodeId) {
        match self.nodes[id].parent {
            Some(p) => self.nodes[p].children.retain(|&c| c != id),
            None => self.roots.retain(|&r| r != id),
        }
    }

    /// Moves every subtree rooted at `other.roots` into `self` as children of
    /// `parent`, remapping node ids. Returns the new child ids.
    pub fn graft(&mut self, other: Document, parent: NodeId) -> Vec<NodeId> {
        let offset = self.nodes.len();
        let roots: Vec<NodeId> = other.roots.iter().map(|r| r + offset).collect();
        for mut el in other.nodes {
            el.parent = Some(el.parent.map_or(parent, |p| p + offset));
            el.children.iter_mut().for_each(|c| *c += offset);
            self.nodes.push(el);
        }
        self.nodes[parent].children.extend(&roots);
        roots
    }
}

fn element_bytes(el: &Element) -> usize {
    el.tag.capacity()
        + el.text.capacity()
        + el.attrs.capacity() * std::mem::size_of::<(String, String)>()
        + el.attrs.iter().map(|(k, v)| k.capacity() + v.capacity()).sum::<usize>()
        + el.children.capacity() * std::mem::size_of::<NodeId>()
}
