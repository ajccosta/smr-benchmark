use circ::{AtomicRc, CsHP, GraphNode, Pointer, Rc, Snapshot, StrongPtr, TaggedCnt};

use super::concurrent_map::{ConcurrentMap, OutputHolder};

use std::{cmp, sync::atomic::Ordering};

static WEIGHT: usize = 2;

// TODO: optimization from the paper? IBR paper doesn't do that

bitflags! {
    /// TODO
    struct Retired: usize {
        const RETIRED = 1usize;
    }
}

impl Retired {
    fn new(retired: bool) -> Self {
        if retired {
            Retired::RETIRED
        } else {
            Retired::empty()
        }
    }

    fn retired(self) -> bool {
        !(self & Retired::RETIRED).is_empty()
    }
}

/// a real node in tree or a wrapper of State node
/// Retired node if Shared ptr of Node has RETIRED tag.
struct Node<K, V> {
    key: K,
    value: V,
    size: usize,
    left: AtomicRc<Node<K, V>, CsHP>,
    right: AtomicRc<Node<K, V>, CsHP>,
}

impl<K, V> GraphNode<CsHP> for Node<K, V> {
    const UNIQUE_OUTDEGREE: bool = false;

    #[inline]
    fn pop_outgoings(&mut self, _: &mut Vec<Rc<Self, CsHP>>)
    where
        Self: Sized,
    {
    }

    #[inline]
    fn pop_unique(&mut self) -> Rc<Self, CsHP>
    where
        Self: Sized,
    {
        unimplemented!()
    }
}

impl<K, V> Node<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    fn retired_node() -> Rc<Self, CsHP> {
        Rc::null().with_tag(Retired::new(true).bits())
    }

    fn is_retired(node: TaggedCnt<Node<K, V>>) -> bool {
        Retired::from_bits_truncate(node.tag()).retired()
    }

    fn is_retired_spot<P>(node: &P) -> bool
    where
        P: StrongPtr<Node<K, V>, CsHP>,
    {
        if Self::is_retired(node.as_ptr()) {
            return true;
        }

        if let Some(node_ref) = node.as_ref() {
            Self::is_retired(node_ref.left.load(Ordering::Acquire))
                || Self::is_retired(node_ref.right.load(Ordering::Acquire))
        } else {
            false
        }
    }

    fn node_size<P>(node: &P) -> usize
    where
        P: StrongPtr<Node<K, V>, CsHP>,
    {
        debug_assert!(!Self::is_retired(node.as_ptr()));
        if let Some(node_ref) = node.as_ref() {
            node_ref.size
        } else {
            0
        }
    }

    fn load_children(&self, cs: &CsHP) -> (Snapshot<Self, CsHP>, Snapshot<Self, CsHP>) {
        let mut left = Snapshot::new();
        left.load(&self.left, cs);
        let mut right = Snapshot::new();
        right.load(&self.right, cs);
        (left, right)
    }
}

pub struct Holder<K, V> {
    root: TaggedCnt<Node<K, V>>,
    curr: Snapshot<Node<K, V>, CsHP>,
    temp: Snapshot<Node<K, V>, CsHP>,
    found: Option<V>,
}

/// Each op creates a new local state and tries to update (CAS) the tree with it.
struct State<'g, K, V> {
    root_link: &'g AtomicRc<Node<K, V>, CsHP>,
    holder: &'g mut Holder<K, V>,
}

pub struct Cursor<K, V> {
    holder: Holder<K, V>,
    /// Temp snapshot to create Rc.
    root_snapshot: Snapshot<Node<K, V>, CsHP>,
}

impl<K, V> OutputHolder<V> for Cursor<K, V> {
    fn default() -> Self {
        Self {
            holder: Holder {
                root: Default::default(),
                curr: Default::default(),
                temp: Default::default(),
                found: None,
            },
            root_snapshot: Default::default(),
        }
    }

    fn output(&self) -> &V {
        self.holder.found.as_ref().unwrap()
    }
}

impl<'g, K, V> State<'g, K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    fn new(root_link: &'g AtomicRc<Node<K, V>, CsHP>, holder: &'g mut Holder<K, V>) -> Self {
        Self { root_link, holder }
    }

    // TODO get ref of K, V and clone here
    fn mk_node<P1, P2>(
        &mut self,
        left: P1,
        right: P2,
        key: K,
        value: V,
        _: &CsHP,
    ) -> Rc<Node<K, V>, CsHP>
    where
        P1: StrongPtr<Node<K, V>, CsHP>,
        P2: StrongPtr<Node<K, V>, CsHP>,
    {
        if Node::is_retired_spot(&left) || Node::is_retired_spot(&right) {
            return Node::retired_node();
        }

        let left_size = Node::node_size(&left);
        let right_size = Node::node_size(&right);

        Rc::new(Node {
            key,
            value,
            size: left_size + right_size + 1,
            left: AtomicRc::from(left.into_rc()),
            right: AtomicRc::from(right.into_rc()),
        })
    }

    /// Make a new balanced tree from cur (the root of a subtree) and newly constructed left and right subtree
    fn mk_balanced<P1, P2, P3>(
        &mut self,
        cur: &P1,
        left: P2,
        right: P3,
        cs: &CsHP,
    ) -> Rc<Node<K, V>, CsHP>
    where
        P1: StrongPtr<Node<K, V>, CsHP>,
        P2: StrongPtr<Node<K, V>, CsHP>,
        P3: StrongPtr<Node<K, V>, CsHP>,
    {
        if Node::is_retired_spot(cur)
            || Node::is_retired_spot(&left)
            || Node::is_retired_spot(&right)
        {
            return Node::retired_node();
        }

        let cur_ref = unsafe { cur.deref() };
        let key = cur_ref.key.clone();
        let value = cur_ref.value.clone();

        let l_size = Node::node_size(&left);
        let r_size = Node::node_size(&right);

        if r_size > 0
            && ((l_size > 0 && r_size > WEIGHT * l_size) || (l_size == 0 && r_size > WEIGHT))
        {
            self.mk_balanced_left(left, right, key, value, cs)
        } else if l_size > 0
            && ((r_size > 0 && l_size > WEIGHT * r_size) || (r_size == 0 && l_size > WEIGHT))
        {
            self.mk_balanced_right(left, right, key, value, cs)
        } else {
            self.mk_node(left, right, key, value, cs)
        }
    }

    #[inline]
    fn mk_balanced_left<P1, P2>(
        &mut self,
        left: P1,
        right: P2,
        key: K,
        value: V,
        cs: &CsHP,
    ) -> Rc<Node<K, V>, CsHP>
    where
        P1: StrongPtr<Node<K, V>, CsHP>,
        P2: StrongPtr<Node<K, V>, CsHP>,
    {
        let right_ref = unsafe { right.deref() };
        let (right_left, right_right) = right_ref.load_children(cs);

        if !self.check_root()
            || Node::is_retired_spot(&right_left)
            || Node::is_retired_spot(&right_right)
        {
            return Node::retired_node();
        }

        if Node::node_size(&right_left) < Node::node_size(&right_right) {
            // single left rotation
            return self.single_left(left, right, right_left, right_right, key, value, cs);
        }

        // double left rotation
        self.double_left(left, right, right_left, right_right, key, value, cs)
    }

    #[inline]
    fn single_left<P1, P2, P3, P4>(
        &mut self,
        left: P1,
        right: P2,
        right_left: P3,
        right_right: P4,
        key: K,
        value: V,
        cs: &CsHP,
    ) -> Rc<Node<K, V>, CsHP>
    where
        P1: StrongPtr<Node<K, V>, CsHP>,
        P2: StrongPtr<Node<K, V>, CsHP>,
        P3: StrongPtr<Node<K, V>, CsHP>,
        P4: StrongPtr<Node<K, V>, CsHP>,
    {
        let right_ref = unsafe { right.deref() };
        let new_left = self.mk_node(left, right_left, key, value, cs);

        self.mk_node(
            new_left,
            right_right,
            right_ref.key.clone(),
            right_ref.value.clone(),
            cs,
        )
    }

    #[inline]
    fn double_left<P1, P2, P3, P4>(
        &mut self,
        left: P1,
        right: P2,
        right_left: P3,
        right_right: P4,
        key: K,
        value: V,
        cs: &CsHP,
    ) -> Rc<Node<K, V>, CsHP>
    where
        P1: StrongPtr<Node<K, V>, CsHP>,
        P2: StrongPtr<Node<K, V>, CsHP>,
        P3: StrongPtr<Node<K, V>, CsHP>,
        P4: StrongPtr<Node<K, V>, CsHP>,
    {
        let right_ref = unsafe { right.deref() };
        let right_left_ref = unsafe { right_left.deref() };
        let (right_left_left, right_left_right) = right_left_ref.load_children(cs);

        if !self.check_root()
            || Node::is_retired_spot(&right_left_left)
            || Node::is_retired_spot(&right_left_right)
        {
            return Node::retired_node();
        }

        let new_left = self.mk_node(left, right_left_left, key, value, cs);
        let new_right = self.mk_node(
            right_left_right,
            right_right,
            right_ref.key.clone(),
            right_ref.value.clone(),
            cs,
        );

        self.mk_node(
            new_left,
            new_right,
            right_left_ref.key.clone(),
            right_left_ref.value.clone(),
            cs,
        )
    }

    #[inline]
    fn mk_balanced_right<P1, P2>(
        &mut self,
        left: P1,
        right: P2,
        key: K,
        value: V,
        cs: &CsHP,
    ) -> Rc<Node<K, V>, CsHP>
    where
        P1: StrongPtr<Node<K, V>, CsHP>,
        P2: StrongPtr<Node<K, V>, CsHP>,
    {
        let left_ref = unsafe { left.deref() };
        let (left_left, left_right) = left_ref.load_children(cs);

        if !self.check_root()
            || Node::is_retired_spot(&left_right)
            || Node::is_retired_spot(&left_left)
        {
            return Node::retired_node();
        }

        if Node::node_size(&left_right) < Node::node_size(&left_left) {
            // single right rotation (fig 3)
            return self.single_right(left, right, left_right, left_left, key, value, cs);
        }
        // double right rotation
        self.double_right(left, right, left_right, left_left, key, value, cs)
    }

    #[inline]
    fn single_right<P1, P2, P3, P4>(
        &mut self,
        left: P1,
        right: P2,
        left_right: P3,
        left_left: P4,
        key: K,
        value: V,
        cs: &CsHP,
    ) -> Rc<Node<K, V>, CsHP>
    where
        P1: StrongPtr<Node<K, V>, CsHP>,
        P2: StrongPtr<Node<K, V>, CsHP>,
        P3: StrongPtr<Node<K, V>, CsHP>,
        P4: StrongPtr<Node<K, V>, CsHP>,
    {
        let left_ref = unsafe { left.deref() };
        let new_right = self.mk_node(left_right, right, key, value, cs);

        self.mk_node(
            left_left,
            new_right,
            left_ref.key.clone(),
            left_ref.value.clone(),
            cs,
        )
    }

    #[inline]
    fn double_right<P1, P2, P3, P4>(
        &mut self,
        left: P1,
        right: P2,
        left_right: P3,
        left_left: P4,
        key: K,
        value: V,
        cs: &CsHP,
    ) -> Rc<Node<K, V>, CsHP>
    where
        P1: StrongPtr<Node<K, V>, CsHP>,
        P2: StrongPtr<Node<K, V>, CsHP>,
        P3: StrongPtr<Node<K, V>, CsHP>,
        P4: StrongPtr<Node<K, V>, CsHP>,
    {
        let left_ref = unsafe { left.deref() };
        let left_right_ref = unsafe { left_right.deref() };
        let (left_right_left, left_right_right) = left_right_ref.load_children(cs);

        if !self.check_root()
            || Node::is_retired_spot(&left_right_left)
            || Node::is_retired_spot(&left_right_right)
        {
            return Node::retired_node();
        }

        let new_left = self.mk_node(
            left_left,
            left_right_left,
            left_ref.key.clone(),
            left_ref.value.clone(),
            cs,
        );
        let new_right = self.mk_node(left_right_right, right, key, value, cs);

        self.mk_node(
            new_left,
            new_right,
            left_right_ref.key.clone(),
            left_right_ref.value.clone(),
            cs,
        )
    }

    #[inline]
    fn do_insert<P>(
        &mut self,
        node: P,
        key: &K,
        value: &V,
        cs: &CsHP,
    ) -> (Rc<Node<K, V>, CsHP>, bool)
    where
        P: StrongPtr<Node<K, V>, CsHP>,
    {
        if Node::is_retired_spot(&node) {
            return (Node::retired_node(), false);
        }

        if node.is_null() {
            return (
                self.mk_node(Rc::null(), Rc::null(), key.clone(), value.clone(), cs),
                true,
            );
        }

        let node_ref = unsafe { node.deref() };
        let (left, right) = node_ref.load_children(cs);

        if !self.check_root() || Node::is_retired_spot(&left) || Node::is_retired_spot(&right) {
            return (Node::retired_node(), false);
        }

        match node_ref.key.cmp(key) {
            cmp::Ordering::Equal => (node.into_rc(), false),
            cmp::Ordering::Less => {
                let (new_right, inserted) = self.do_insert(right, key, value, cs);
                (self.mk_balanced(&node, left, new_right, cs), inserted)
            }
            cmp::Ordering::Greater => {
                let (new_left, inserted) = self.do_insert(left, key, value, cs);
                (self.mk_balanced(&node, new_left, right, cs), inserted)
            }
        }
    }

    #[inline]
    fn do_remove<P>(&mut self, node: P, key: &K, cs: &CsHP) -> (Rc<Node<K, V>, CsHP>, bool)
    where
        P: StrongPtr<Node<K, V>, CsHP>,
    {
        if Node::is_retired_spot(&node) {
            return (Node::retired_node(), false);
        }

        if node.is_null() {
            return (Rc::null(), false);
        }

        let node_ref = unsafe { node.deref() };
        let (left, right) = node_ref.load_children(cs);

        if !self.check_root() || Node::is_retired_spot(&left) || Node::is_retired_spot(&right) {
            return (Node::retired_node(), false);
        }

        match node_ref.key.cmp(key) {
            cmp::Ordering::Equal => {
                self.holder.found = Some(node_ref.value.clone());
                if node_ref.size == 1 {
                    return (Rc::null(), true);
                }

                if !left.is_null() {
                    let (new_left, succ) = self.pull_rightmost(left, cs);
                    return (self.mk_balanced(&succ, new_left, right, cs), true);
                }
                let (new_right, succ) = self.pull_leftmost(right, cs);
                (self.mk_balanced(&succ, left, new_right, cs), true)
            }
            cmp::Ordering::Less => {
                let (new_right, found) = self.do_remove(right, key, cs);
                (self.mk_balanced(&node, left, new_right, cs), found)
            }
            cmp::Ordering::Greater => {
                let (new_left, found) = self.do_remove(left, key, cs);
                (self.mk_balanced(&node, new_left, right, cs), found)
            }
        }
    }

    fn pull_leftmost<P>(
        &mut self,
        node: P,
        cs: &CsHP,
    ) -> (Rc<Node<K, V>, CsHP>, Rc<Node<K, V>, CsHP>)
    where
        P: StrongPtr<Node<K, V>, CsHP>,
    {
        if Node::is_retired_spot(&node) {
            return (Node::retired_node(), Node::retired_node());
        }

        let node_ref = unsafe { node.deref() };
        let (left, right) = node_ref.load_children(cs);

        if !self.check_root() || Node::is_retired_spot(&left) || Node::is_retired_spot(&right) {
            return (Node::retired_node(), Node::retired_node());
        }

        if !left.is_null() {
            let (new_left, succ) = self.pull_leftmost(left, cs);
            return (self.mk_balanced(&node, new_left, right, cs), succ);
        }
        // node is the leftmost
        let succ = self.mk_node(
            Rc::null(),
            Rc::null(),
            node_ref.key.clone(),
            node_ref.value.clone(),
            cs,
        );
        (right.into_rc(), succ)
    }

    fn pull_rightmost<P>(
        &mut self,
        node: P,
        cs: &CsHP,
    ) -> (Rc<Node<K, V>, CsHP>, Rc<Node<K, V>, CsHP>)
    where
        P: StrongPtr<Node<K, V>, CsHP>,
    {
        if Node::is_retired_spot(&node) {
            return (Node::retired_node(), Node::retired_node());
        }

        let node_ref = unsafe { node.deref() };
        let (left, right) = node_ref.load_children(cs);

        if !self.check_root() || Node::is_retired_spot(&left) || Node::is_retired_spot(&right) {
            return (Node::retired_node(), Node::retired_node());
        }

        if !right.is_null() {
            let (new_right, succ) = self.pull_rightmost(right, cs);
            return (self.mk_balanced(&node, left, new_right, cs), succ);
        }
        // node is the rightmost
        let succ = self.mk_node(
            Rc::null(),
            Rc::null(),
            node_ref.key.clone(),
            node_ref.value.clone(),
            cs,
        );
        (left.into_rc(), succ)
    }

    pub fn check_root(&self) -> bool {
        self.holder.root == self.root_link.load(Ordering::Acquire)
    }
}

pub struct BonsaiTreeMap<K, V> {
    root: AtomicRc<Node<K, V>, CsHP>,
}

impl<K, V> Default for BonsaiTreeMap<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> BonsaiTreeMap<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    pub fn new() -> Self {
        Self {
            root: AtomicRc::null(),
        }
    }

    pub fn get(&self, key: &K, holder: &mut Holder<K, V>, cs: &CsHP) -> bool {
        loop {
            // NOTE: In this context, `holder.curr` and `holder.temp` is similar
            // to `curr` and `next` in a HHSList traversal.
            holder.curr.load(&self.root, cs);
            loop {
                let curr_node = some_or!(holder.curr.as_ref(), return false);
                let next_link = match key.cmp(&curr_node.key) {
                    cmp::Ordering::Equal => break,
                    cmp::Ordering::Less => &curr_node.left,
                    cmp::Ordering::Greater => &curr_node.right,
                };
                holder.temp.load(next_link, cs);
                Snapshot::swap(&mut holder.curr, &mut holder.temp);
            }

            if Node::is_retired_spot(&holder.curr) {
                continue;
            }

            if holder.curr.is_null() {
                return false;
            }

            holder.found = Some(unsafe { holder.curr.deref() }.value.clone());
            return true;
        }
    }

    pub fn insert(&self, key: K, value: V, cursor: &mut Cursor<K, V>, cs: &CsHP) -> bool {
        let mut state = State::new(&self.root, &mut cursor.holder);
        loop {
            cursor.root_snapshot.load(&self.root, cs);
            state.holder.root = cursor.root_snapshot.as_ptr();
            let (new_root, inserted) = state.do_insert(&cursor.root_snapshot, &key, &value, cs);

            if Node::is_retired(new_root.as_ptr()) {
                continue;
            }

            if self
                .root
                .compare_exchange(
                    cursor.root_snapshot.as_ptr(),
                    new_root,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    cs,
                )
                .is_ok()
            {
                return inserted;
            }
        }
    }

    pub fn remove(&self, key: &K, cursor: &mut Cursor<K, V>, cs: &CsHP) -> bool {
        let mut state = State::new(&self.root, &mut cursor.holder);
        loop {
            cursor.root_snapshot.load(&self.root, cs);
            state.holder.root = cursor.root_snapshot.as_ptr();
            let (new_root, found) = state.do_remove(&cursor.root_snapshot, key, cs);

            if Node::is_retired(new_root.as_ptr()) {
                continue;
            }

            if self
                .root
                .compare_exchange(
                    cursor.root_snapshot.as_ptr(),
                    new_root,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    cs,
                )
                .is_ok()
            {
                return found;
            }
        }
    }
}

impl<K, V> ConcurrentMap<K, V> for BonsaiTreeMap<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    type Output = Cursor<K, V>;

    fn new() -> Self {
        BonsaiTreeMap::new()
    }

    fn get(&self, key: &K, output: &mut Self::Output, cs: &CsHP) -> bool {
        self.get(key, &mut output.holder, cs)
    }

    fn insert(&self, key: K, value: V, output: &mut Self::Output, cs: &CsHP) -> bool {
        self.insert(key, value, output, cs)
    }

    fn remove(&self, key: &K, output: &mut Self::Output, cs: &CsHP) -> bool {
        self.remove(key, output, cs)
    }
}

#[cfg(test)]
mod tests {
    use super::BonsaiTreeMap;
    use crate::ds_impl::circ_hp::concurrent_map;

    #[test]
    fn smoke_bonsai_tree() {
        concurrent_map::tests::smoke::<_, BonsaiTreeMap<i32, String>, _>(&|a| a.to_string());
    }
}
