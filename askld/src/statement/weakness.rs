//! Which non-constraining substatements are strong.
//!
//! The query graph of one top-level statement has a vertex per substatement
//! and an undirected edge per scope nesting and per label use inside that
//! statement.  Constraining substatements (the *terminals*) are always
//! strong.  The rule for the rest:
//!
//! > A non-constraining vertex is strong iff it reaches two different
//! > terminals along two routes that share no vertex except itself.
//!
//! Joined at the vertex, two such routes are a simple path between two
//! terminals through it, so the strong vertices are exactly the union of
//! all simple terminal-to-terminal paths: the strong part of a statement is
//! one connected component.  (A *minimum* Steiner tree would be NP-hard and
//! not unique; this union is unique and order-independent.)
//!
//! Plain reachability is not enough: in `"a" { func ; "b" }` both routes from
//! `func` leave through `"a"`, so `func` bridges nothing and stays weak.
//!
//! Per candidate the check is a vertex-capacitated max-flow from the
//! candidate to the terminal set, stopped at 2.  Promoting a vertex never
//! changes another vertex's answer — a promoted vertex already lies on a
//! path between two terminals, so a route ending at it extends to one of
//! them — so one pass over the candidates decides all of them.

use std::collections::VecDeque;

/// For each vertex, whether it is strong.  `terminal[v]` marks the
/// constraining vertices (always strong); `edges` are undirected.
pub(crate) fn strong_vertices(terminal: &[bool], edges: &[(usize, usize)]) -> Vec<bool> {
    let n = terminal.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a != b {
            adj[a].push(b);
            adj[b].push(a);
        }
    }
    (0..n)
        .map(|v| terminal[v] || has_two_disjoint_routes(v, terminal, &adj))
        .collect()
}

/// Residual graph for a small max-flow.
struct Flow {
    to: Vec<usize>,
    cap: Vec<i32>,
    head: Vec<Vec<usize>>,
}

impl Flow {
    fn new(nodes: usize) -> Self {
        Flow {
            to: Vec::new(),
            cap: Vec::new(),
            head: vec![Vec::new(); nodes],
        }
    }

    fn add(&mut self, a: usize, b: usize, c: i32) {
        self.head[a].push(self.to.len());
        self.to.push(b);
        self.cap.push(c);
        self.head[b].push(self.to.len());
        self.to.push(a);
        self.cap.push(0);
    }

    /// One BFS augmentation of one unit; false when no path is left.
    fn augment(&mut self, s: usize, t: usize) -> bool {
        let mut via: Vec<Option<usize>> = vec![None; self.head.len()];
        let mut seen = vec![false; self.head.len()];
        seen[s] = true;
        let mut queue = VecDeque::from([s]);
        while let Some(x) = queue.pop_front() {
            if x == t {
                break;
            }
            for &e in &self.head[x] {
                let y = self.to[e];
                if self.cap[e] > 0 && !seen[y] {
                    seen[y] = true;
                    via[y] = Some(e);
                    queue.push_back(y);
                }
            }
        }
        if !seen[t] {
            return false;
        }
        let mut y = t;
        while let Some(e) = via[y] {
            self.cap[e] -= 1;
            self.cap[e ^ 1] += 1;
            y = self.to[e ^ 1];
        }
        true
    }
}

/// Two routes from `v` to two different terminals, sharing only `v`.
///
/// Vertex x is split into `in(x) = 2x` and `out(x) = 2x + 1`.  A
/// non-terminal x other than `v` passes one unit (`in → out`, capacity 1);
/// a terminal ends a route (`in → sink`, capacity 1) and passes nothing, so
/// two routes can neither share an intermediate vertex nor end at the same
/// terminal.  Routes that would run through a terminal to a farther one
/// are covered by stopping at the first.
fn has_two_disjoint_routes(v: usize, terminal: &[bool], adj: &[Vec<usize>]) -> bool {
    let n = terminal.len();
    let sink = 2 * n;
    let mut flow = Flow::new(2 * n + 1);
    for x in 0..n {
        if x == v {
            continue;
        }
        if terminal[x] {
            flow.add(2 * x, sink, 1);
        } else {
            flow.add(2 * x, 2 * x + 1, 1);
        }
    }
    for x in 0..n {
        if terminal[x] && x != v {
            continue; // a route never leaves a terminal
        }
        for &y in &adj[x] {
            if y != v {
                flow.add(2 * x + 1, 2 * y, 1);
            }
        }
    }
    let source = 2 * v + 1;
    flow.augment(source, sink) && flow.augment(source, sink)
}

#[cfg(test)]
mod tests {
    use super::strong_vertices;

    /// `t` lists the terminals; the result lists the strong NON-terminals.
    fn strong(n: usize, t: &[usize], edges: &[(usize, usize)]) -> Vec<usize> {
        let mut terminal = vec![false; n];
        for &x in t {
            terminal[x] = true;
        }
        let s = strong_vertices(&terminal, edges);
        (0..n).filter(|&v| s[v] && !terminal[v]).collect()
    }

    #[test]
    fn terminals_are_always_strong_and_nothing_else_without_a_pair() {
        assert!(strong_vertices(&[true], &[]) == vec![true]);
        assert_eq!(strong(3, &[], &[(0, 1), (1, 2)]), Vec::<usize>::new());
        assert_eq!(strong(3, &[0], &[(0, 1), (1, 2)]), Vec::<usize>::new());
        let s = strong_vertices(&[false, true, false], &[(0, 1), (1, 2)]);
        assert_eq!(s, vec![false, true, false]);
    }

    #[test]
    fn a_path_between_two_terminals_is_strong() {
        // s - v - t
        assert_eq!(strong(3, &[0, 2], &[(0, 1), (1, 2)]), vec![1]);
        // s - v - w - t
        assert_eq!(strong(4, &[0, 3], &[(0, 1), (1, 2), (2, 3)]), vec![1, 2]);
    }

    #[test]
    fn a_star_bridges_only_two_terminal_leaves() {
        // centre 0; leaves 1..=3
        let star = [(0, 1), (0, 2), (0, 3)];
        assert_eq!(strong(4, &[1, 2], &star), vec![0]);
        assert_eq!(strong(4, &[1], &star), Vec::<usize>::new());
    }

    #[test]
    fn a_pendant_off_a_terminal_cut_vertex_stays_weak() {
        // `"a" { func ; "b" }`: a=0, func=1, b=2
        assert_eq!(strong(3, &[0, 2], &[(0, 1), (0, 2)]), Vec::<usize>::new());
    }

    #[test]
    fn a_pendant_chain_off_a_strong_bridge_stays_weak() {
        // s - v - t, with v - w - x hanging off v
        let edges = [(0, 1), (1, 2), (1, 3), (3, 4)];
        assert_eq!(strong(5, &[0, 2], &edges), vec![1]);
    }

    #[test]
    fn a_cycle_through_two_terminals_is_strong_but_its_pendant_is_not() {
        // cycle 0-1-2-3-0, terminals 0 and 2, pendant 4 off 1
        let edges = [(0, 1), (1, 2), (2, 3), (3, 0), (1, 4)];
        assert_eq!(strong(5, &[0, 2], &edges), vec![1, 3]);
    }

    #[test]
    fn a_cycle_with_one_terminal_bridged_through_a_cut_vertex() {
        // cycle 0-1-2-0 holds terminal 0; vertex 2 leads on to terminal 4
        // via 3.  Every cycle vertex is on some simple 0..4 path: 0-2-3-4
        // and 0-1-2-3-4.
        let edges = [(0, 1), (1, 2), (2, 0), (2, 3), (3, 4)];
        assert_eq!(strong(5, &[0, 4], &edges), vec![1, 2, 3]);
    }

    #[test]
    fn a_second_cycle_without_terminals_stays_weak() {
        // cycles 0-1-2-0 and 2-3-4-2 share cut vertex 2; terminals 0, 1
        let edges = [(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 2)];
        assert_eq!(strong(5, &[0, 1], &edges), vec![2]);
    }

    #[test]
    fn a_terminal_cut_vertex_does_not_bridge_for_others() {
        // s - v - t - w - u, terminals s, t, u: v and w each bridge a pair
        let edges = [(0, 1), (1, 2), (2, 3), (3, 4)];
        assert_eq!(strong(5, &[0, 2, 4], &edges), vec![1, 3]);
        // v hanging off terminal t between two other terminals stays weak
        let edges = [(0, 2), (2, 4), (2, 1)];
        assert_eq!(strong(5, &[0, 2, 4], &edges), Vec::<usize>::new());
    }

    #[test]
    fn the_label_cycle_of_two_branches_sharing_a_callee() {
        // `func { "a" { @c } ; "b" { #c } }`
        // func=0, "a"=1, @c=2, "b"=3, #c=4; label edge @c - #c.
        let edges = [(0, 1), (1, 2), (0, 3), (3, 4), (4, 2)];
        assert_eq!(strong(5, &[1, 3, 4], &edges), vec![0, 2]);
    }

    // ---- oracle -----------------------------------------------------------

    /// Brute force: v is strong iff some simple path between two different
    /// terminals passes through v.
    fn oracle(terminal: &[bool], edges: &[(usize, usize)]) -> Vec<bool> {
        let n = terminal.len();
        let mut adj = vec![Vec::new(); n];
        for &(a, b) in edges {
            if a != b {
                adj[a].push(b);
                adj[b].push(a);
            }
        }
        let mut on_path = terminal.to_vec();
        fn walk(
            x: usize,
            start: usize,
            adj: &[Vec<usize>],
            terminal: &[bool],
            path: &mut Vec<usize>,
            seen: &mut [bool],
            on_path: &mut [bool],
        ) {
            if x != start && terminal[x] {
                for &p in path.iter() {
                    on_path[p] = true;
                }
                return; // a longer path through a terminal is two shorter ones
            }
            for &y in &adj[x] {
                if !seen[y] {
                    seen[y] = true;
                    path.push(y);
                    walk(y, start, adj, terminal, path, seen, on_path);
                    path.pop();
                    seen[y] = false;
                }
            }
        }
        for s in 0..n {
            if !terminal[s] {
                continue;
            }
            let mut seen = vec![false; n];
            seen[s] = true;
            let mut path = vec![s];
            walk(s, s, &adj, terminal, &mut path, &mut seen, &mut on_path);
        }
        on_path
    }

    fn all_pairs(n: usize) -> Vec<(usize, usize)> {
        let mut pairs = Vec::new();
        for a in 0..n {
            for b in a + 1..n {
                pairs.push((a, b));
            }
        }
        pairs
    }

    #[test]
    fn matches_the_brute_force_oracle_on_every_small_graph() {
        for n in 1..=5 {
            let pairs = all_pairs(n);
            for edge_mask in 0u32..(1 << pairs.len()) {
                let edges: Vec<_> = pairs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| edge_mask & (1 << i) != 0)
                    .map(|(_, &e)| e)
                    .collect();
                for term_mask in 0u32..(1 << n) {
                    let terminal: Vec<bool> = (0..n).map(|v| term_mask & (1 << v) != 0).collect();
                    assert_eq!(
                        strong_vertices(&terminal, &edges),
                        oracle(&terminal, &edges),
                        "n={n} edges={edges:?} terminals={terminal:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn matches_the_oracle_on_random_graphs_and_under_relabelling() {
        // Deterministic LCG: no dev-dependency, same graphs every run.
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        for _ in 0..400 {
            let n = 6 + next(3) as usize; // 6..=8
            let pairs = all_pairs(n);
            let edges: Vec<_> = pairs.into_iter().filter(|_| next(100) < 35).collect();
            let terminal: Vec<bool> = (0..n).map(|_| next(100) < 35).collect();
            let got = strong_vertices(&terminal, &edges);
            assert_eq!(
                got,
                oracle(&terminal, &edges),
                "edges={edges:?} t={terminal:?}"
            );

            // Relabel by a rotation and check the answer moves with it.
            let k = 1 + next(n as u64 - 1) as usize;
            let rel = |x: usize| (x + k) % n;
            let edges2: Vec<_> = edges.iter().map(|&(a, b)| (rel(a), rel(b))).collect();
            let mut terminal2 = vec![false; n];
            for v in 0..n {
                terminal2[rel(v)] = terminal[v];
            }
            let got2 = strong_vertices(&terminal2, &edges2);
            for v in 0..n {
                assert_eq!(got[v], got2[rel(v)]);
            }
        }
    }
}
