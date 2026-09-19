//! The lazy S3 browse tree: node model, the async listing that fills it in
//! on expand, and the `TreeItem` projection `build_tree_items` hangs off the
//! schema tree's S3 root.

use std::collections::HashMap;

use gpui_kit::component::tree::{TreeEvent, TreeItem};
use gpui_kit::{Context, SharedString};

use super::model::{SchemaNodeKind, SchemaNodeMeta};
use super::Sidebar;
use crate::i18n::{tr, trf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum S3NodeKind {
    Bucket,
    Prefix,
    File,
}

/// Lazily loaded children of an S3 tree node (root, bucket, or prefix).
#[derive(Clone)]
pub(super) enum S3Children {
    NotLoaded,
    Loading,
    Loaded(Vec<S3Node>),
    Failed(String),
}

/// One entry of the S3 browse tree kept by the sidebar.
#[derive(Clone)]
pub(super) struct S3Node {
    id: SharedString,
    label: String,
    kind: S3NodeKind,
    /// Bucket + prefix used to list this node's children on expand.
    bucket: String,
    prefix: String,
    /// `s3://bucket/key` for files, used to generate the SELECT query.
    s3_uri: Option<String>,
    detail: Option<String>,
    children: S3Children,
    expanded: bool,
}

impl S3Node {
    fn bucket_node(name: String) -> Self {
        Self {
            id: format!("s3:bucket:{name}").into(),
            label: name.clone(),
            kind: S3NodeKind::Bucket,
            bucket: name,
            prefix: String::new(),
            s3_uri: None,
            detail: None,
            children: S3Children::NotLoaded,
            expanded: false,
        }
    }

    fn prefix_node(bucket: &str, prefix: String) -> Self {
        let label = prefix
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&prefix)
            .to_string();
        Self {
            id: format!("s3:prefix:{bucket}/{prefix}").into(),
            label,
            kind: S3NodeKind::Prefix,
            bucket: bucket.to_string(),
            prefix,
            s3_uri: None,
            detail: None,
            children: S3Children::NotLoaded,
            expanded: false,
        }
    }

    fn file_node(bucket: &str, key: String, size: i64) -> Self {
        let label = key.rsplit('/').next().unwrap_or(&key).to_string();
        Self {
            id: format!("s3:file:{bucket}/{key}").into(),
            label,
            kind: S3NodeKind::File,
            bucket: bucket.to_string(),
            prefix: String::new(),
            // Only data files DuckDB can read directly get a click-to-query action.
            s3_uri: crate::db::is_data_file(&key).then(|| format!("s3://{bucket}/{key}")),
            detail: Some(format_size(size)),
            children: S3Children::NotLoaded,
            expanded: false,
        }
    }
}

/// Browse state of the S3 root node, kept across tree rebuilds.
#[derive(Clone)]
pub(super) struct S3Browse {
    pub(super) expanded: bool,
    pub(super) children: S3Children,
}

impl Default for S3Browse {
    fn default() -> Self {
        Self {
            expanded: false,
            children: S3Children::NotLoaded,
        }
    }
}

/// Which node's listing an in-flight S3 request belongs to.
#[derive(Clone)]
enum S3LoadTarget {
    Root,
    Node(SharedString),
}

/// Human-readable object size for the sidebar detail column.
fn format_size(bytes: i64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / (1 << 20) as f64)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

impl Sidebar {
    /// Track expansion of S3 nodes and kick off lazy listing on first expand.
    pub(super) fn on_tree_event(&mut self, event: &TreeEvent, cx: &mut Context<Self>) {
        let (id, expanded) = match event {
            TreeEvent::Expanded(id) => (id, true),
            TreeEvent::Collapsed(id) => (id, false),
        };
        if !id.starts_with("s3:") {
            return;
        }
        let Some(browse) = self.s3_browse.as_mut() else {
            return;
        };
        let load = if id == "s3:root" {
            browse.expanded = expanded;
            if expanded && matches!(browse.children, S3Children::NotLoaded) {
                browse.children = S3Children::Loading;
                Some(S3LoadTarget::Root)
            } else {
                None
            }
        } else if let Some(node) = find_s3_node_mut(&mut browse.children, id) {
            node.expanded = expanded;
            if expanded && matches!(node.children, S3Children::NotLoaded) {
                node.children = S3Children::Loading;
                Some(S3LoadTarget::Node(node.id.clone()))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(target) = load {
            self.rebuild_tree(cx);
            self.start_s3_load(target, cx);
        }
    }

    /// Fire the blocking list call for a node that just entered `Loading`.
    fn start_s3_load(&mut self, target: S3LoadTarget, cx: &mut Context<Self>) {
        let Some(config) = self.state.read(cx).s3_config.clone() else {
            return;
        };
        let request = match &target {
            S3LoadTarget::Root => None,
            S3LoadTarget::Node(id) => self.s3_browse.as_ref().and_then(|browse| {
                find_s3_node(&browse.children, id)
                    .map(|node| (node.bucket.clone(), node.prefix.clone()))
            }),
        };
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || -> anyhow::Result<Vec<S3Node>> {
                match &request {
                    None => Ok(crate::s3::list_buckets(&config)?
                        .into_iter()
                        .map(S3Node::bucket_node)
                        .collect()),
                    Some((bucket, prefix)) => {
                        let listing = crate::s3::list_objects(&config, bucket, prefix)?;
                        let mut nodes: Vec<S3Node> = listing
                            .prefixes
                            .into_iter()
                            .map(|prefix| S3Node::prefix_node(bucket, prefix))
                            .collect();
                        nodes.extend(
                            listing
                                .objects
                                .into_iter()
                                .map(|object| S3Node::file_node(bucket, object.key, object.size)),
                        );
                        Ok(nodes)
                    }
                }
            })
            .await;
            this.update(cx, |this, cx| {
                this.finish_s3_load(target, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_s3_load(
        &mut self,
        target: S3LoadTarget,
        result: anyhow::Result<Vec<S3Node>>,
        cx: &mut Context<Self>,
    ) {
        let children = match result {
            Ok(nodes) => S3Children::Loaded(nodes),
            Err(e) => S3Children::Failed(trf("sidebar.s3.load_failed", &[&e.to_string()])),
        };
        if let Some(browse) = self.s3_browse.as_mut() {
            match target {
                S3LoadTarget::Root => browse.children = children,
                S3LoadTarget::Node(id) => {
                    if let Some(node) = find_s3_node_mut(&mut browse.children, &id) {
                        node.children = children;
                    }
                }
            }
        }
        self.rebuild_tree(cx);
    }

    /// Reload the bucket list (also the retry path after a failure).
    pub(super) fn refresh_s3(&mut self, cx: &mut Context<Self>) {
        if let Some(browse) = self.s3_browse.as_mut() {
            browse.expanded = true;
            browse.children = S3Children::Loading;
        }
        self.rebuild_tree(cx);
        self.start_s3_load(S3LoadTarget::Root, cx);
    }
}

fn find_s3_node<'a>(children: &'a S3Children, id: &str) -> Option<&'a S3Node> {
    fn find_in<'a>(nodes: &'a [S3Node], id: &str) -> Option<&'a S3Node> {
        for node in nodes {
            if node.id.as_ref() == id {
                return Some(node);
            }
            if let Some(found) = find_s3_node(&node.children, id) {
                return Some(found);
            }
        }
        None
    }
    match children {
        S3Children::Loaded(nodes) => find_in(nodes, id),
        _ => None,
    }
}

fn find_s3_node_mut<'a>(children: &'a mut S3Children, id: &str) -> Option<&'a mut S3Node> {
    fn find_in<'a>(nodes: &'a mut [S3Node], id: &str) -> Option<&'a mut S3Node> {
        for node in nodes {
            if node.id.as_ref() == id {
                return Some(node);
            }
            if let S3Children::Loaded(children) = &mut node.children {
                if let Some(found) = find_in(children, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    match children {
        S3Children::Loaded(nodes) => find_in(nodes, id),
        _ => None,
    }
}

/// Children of an S3 node: loading/error placeholders until the listing
/// arrives. An unloaded node still needs one (disabled) child so the tree
/// treats it as an expandable folder.
pub(super) fn s3_children_items(
    parent_id: &str,
    children: &S3Children,
    meta: &mut HashMap<SharedString, SchemaNodeMeta>,
) -> Vec<TreeItem> {
    let mut placeholder = |suffix: &str, label: String| {
        let id: SharedString = format!("{parent_id}/{suffix}").into();
        meta.insert(
            id.clone(),
            SchemaNodeMeta::new(SchemaNodeKind::S3Message, None),
        );
        TreeItem::new(id, label).disabled(true)
    };
    match children {
        S3Children::NotLoaded | S3Children::Loading => {
            vec![placeholder("loading", tr("sidebar.s3.loading").to_string())]
        }
        S3Children::Failed(message) => vec![placeholder("error", message.clone())],
        S3Children::Loaded(nodes) => {
            if nodes.is_empty() {
                vec![placeholder("empty", tr("sidebar.s3.empty").to_string())]
            } else {
                nodes.iter().map(|node| s3_node_item(node, meta)).collect()
            }
        }
    }
}

fn s3_node_item(node: &S3Node, meta: &mut HashMap<SharedString, SchemaNodeMeta>) -> TreeItem {
    let kind = match node.kind {
        S3NodeKind::Bucket => SchemaNodeKind::S3Bucket,
        S3NodeKind::Prefix => SchemaNodeKind::S3Prefix,
        S3NodeKind::File => SchemaNodeKind::S3File,
    };
    meta.insert(
        node.id.clone(),
        SchemaNodeMeta {
            s3_uri: node.s3_uri.clone(),
            ..SchemaNodeMeta::new(kind, node.detail.clone().map(Into::into))
        },
    );
    let children = match node.kind {
        S3NodeKind::File => Vec::new(),
        _ => s3_children_items(&node.id, &node.children, meta),
    };
    TreeItem::new(node.id.clone(), node.label.clone())
        .expanded(node.expanded)
        .children(children)
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: a `gpui_kit::*` glob anywhere in the
    // import chain drags gpui's `test` attribute macro into scope and shadows
    // the built-in `#[test]`.
    use super::{find_s3_node, format_size, S3Children, S3Node, S3NodeKind};

    #[test]
    fn s3_node_labels_and_ids() {
        let bucket = S3Node::bucket_node("logs".to_string());
        assert_eq!(bucket.id.as_ref(), "s3:bucket:logs");
        assert!(matches!(bucket.children, S3Children::NotLoaded));

        let prefix = S3Node::prefix_node("logs", "2024/09/".to_string());
        assert_eq!(prefix.id.as_ref(), "s3:prefix:logs/2024/09/");
        assert_eq!(prefix.label, "09");
        assert_eq!(prefix.prefix, "2024/09/");

        let file = S3Node::file_node("logs", "2024/09/data.parquet".to_string(), 2048);
        assert_eq!(file.id.as_ref(), "s3:file:logs/2024/09/data.parquet");
        assert_eq!(file.label, "data.parquet");
        assert_eq!(file.kind, S3NodeKind::File);
        assert_eq!(
            file.s3_uri.as_deref(),
            Some("s3://logs/2024/09/data.parquet")
        );
        assert_eq!(file.detail.as_deref(), Some("2 KB"));
    }

    #[test]
    fn find_s3_node_walks_loaded_subtrees() {
        let mut child = S3Node::prefix_node("b", "x/".to_string());
        child.children = S3Children::Loaded(vec![S3Node::file_node("b", "x/f.csv".to_string(), 1)]);
        let mut root = S3Node::bucket_node("b".to_string());
        root.children = S3Children::Loaded(vec![child]);
        let tree = S3Children::Loaded(vec![root]);

        assert!(find_s3_node(&tree, "s3:file:b/x/f.csv").is_some());
        assert!(find_s3_node(&tree, "s3:file:b/missing.csv").is_none());
        // Unloaded subtrees are opaque to the search.
        assert!(find_s3_node(&S3Children::NotLoaded, "s3:bucket:b").is_none());
    }

    #[test]
    fn format_size_picks_human_units() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
