#!/usr/bin/env python3
"""Generate benchmark queries in GQL format for graph shape patterns.

Shapes from the CompactLTJ paper (Section 5.5), expressed in two styles:
1. Linear path patterns (using Concat)
2. Join patterns using comma (Q1, Q2) for stars and cliques

Usage: python generate_queries.py <output_dir>
"""

import os
import sys

# All shapes as GQL path patterns.
# The graph is unlabeled and directed: edges are -[]->.
SHAPES = {
    # --- Paths (linear) ---
    "2-comb": [
        "(a) -[]-> (b) -[]-> (c)",
    ],
    "3-path": [
        "(a) -[]-> (b) -[]-> (c) -[]-> (d)",
    ],
    "4-path": [
        "(a) -[]-> (b) -[]-> (c) -[]-> (d) -[]-> (e)",
    ],

    # --- Trees (star via comma-join) ---
    # 1-tree: node with 2 outgoing edges
    "1-tree": [
        "(a) -[]-> (b), (a) -[]-> (c)",
    ],
    # 2-tree: two connected stars
    "2-tree": [
        "(a) -[]-> (b), (a) -[]-> (c), (b) -[]-> (d)",
    ],

    # --- Cycles (via comma-join for proper cycle closing) ---
    # 3-cycle / triangle
    "3-cycle": [
        "(a) -[]-> (b), (b) -[]-> (c), (c) -[]-> (a)",
    ],
    # 4-cycle
    "4-cycle": [
        "(a) -[]-> (b), (b) -[]-> (c), (c) -[]-> (d), (d) -[]-> (a)",
    ],

    # --- Cliques (via comma-join for all edges) ---
    # 3-clique: triangle (all 3 directed edges)
    "3-clique": [
        "(a) -[]-> (b), (b) -[]-> (c), (c) -[]-> (a)",
    ],
    # 4-clique: all 6 directed edges between 4 nodes
    "4-clique": [
        "(a) -[]-> (b), (a) -[]-> (c), (a) -[]-> (d), (b) -[]-> (c), (b) -[]-> (d), (c) -[]-> (d)",
    ],

    # --- Lollipops ---
    # 2-3-lollipop: triangle + 2-path tail from one vertex
    "2-3-lollipop": [
        "(a) -[]-> (b), (b) -[]-> (c), (c) -[]-> (a), (c) -[]-> (d) -[]-> (e)",
    ],
    # 3-4-lollipop: 4-cycle + 3-path tail
    "3-4-lollipop": [
        "(a) -[]-> (b), (b) -[]-> (c), (c) -[]-> (d), (d) -[]-> (a), (d) -[]-> (e) -[]-> (f) -[]-> (g)",
    ],
}


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <output_dir>")
        sys.exit(1)

    output_dir = sys.argv[1]
    os.makedirs(output_dir, exist_ok=True)

    for shape_name, queries in SHAPES.items():
        out_path = os.path.join(output_dir, f"{shape_name}.gql")
        with open(out_path, "w") as f:
            for q in queries:
                f.write(q + "\n")
        print(f"{shape_name}: {len(queries)} queries -> {out_path}")

    print(f"\nGenerated {len(SHAPES)} query files in {output_dir}/")


if __name__ == "__main__":
    main()
