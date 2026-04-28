# Formal Specifications for FPPC (Flexible Path Pattern Calculus)

## 1. Core Architecture and Philosophy
The Flexible Path Pattern Calculus (FPPC) provides a formal type model for a core fragment of the Graph Query Language (GQL). It extends standard GQL by introducing property-based filtering and gradual types to handle incomplete or evolving schema information. 

The system is designed to statically detect queries that are semantically incorrect, prone to execution failure (stuck states), or guaranteed to return empty results. It achieves flexibility through the "unknown type" ($\star$), which can represent any label or property when type information is imprecise.

---

## 2. Syntax of Types
The type system is built on a hierarchy of types, from basic scalar values to complex graph paths.

### Data Types
* **Base Types ($\iota$):** $\mathbb{Z} \mid \mathbb{B} \mid \text{String}$
* **Simple Types ($\tau$):** $\iota \mid \star \mid \tau + \tau \mid \perp$ (Includes base types, unknown type, gradual unions, and bottom/error type).
* **Property Types ($R$):** * Closed Records: $\{\overline{a_i : \tau_i}^i\}$
    * Open Records: $\{\overline{a_i : \tau_i}^i, \star\}$
    * Bottom: $\perp$
* **Label Types ($\ell$):** $1 \mid \star \mid \ell \& \ell \mid \ell + \ell \mid \epsilon$ (Concrete label, unknown, intersection, union, and empty label acting as universal supertype).
* **Descriptor Types ($L$):** $\ell \ R$ (Combines a label type and a property type to define atomic matching conditions for nodes and edges).

### Graph Types
* **Node Types ($N$):** $(| L |)$
* **Edge Types ($E$):** $N \rightarrow N \mid N \sim N$ (Directed and undirected edges).
* **Variable Types ($T$):** $N \mid E \mid T + T \mid [T]^l \mid \perp \mid \text{Null}$ (Assigns types to nodes/edges captured by variables. Includes lists with minimum cardinality bounds).
* **Path Types ($P$):** $N \mid P - N \mid P + P \mid \perp$ (Specifies first and last node types and encodes intermediate node information for concatenation).

---

## 3. Subtyping Rules ($A \le B$)
**CRITICAL CLARIFICATION: Plausibility-Based Gradual Subtyping**
FPPC uses *purely gradual* subtyping, not classical subtyping. For union types, FPPC is optimistic: $A + B \le C$ is true if *at least one* of the components is a subtype of $C$ (e.g., $A \le C$). Classical systems strictly require *both* to be subtypes. 

### 3.1 Label Types ($\ell_1 \le \ell_2$)
* **Base Rules:** Identical labels are subtypes ($1 \le 1$). The unknown label ($\star$) is both a subtype and supertype of any label ($\star \le \ell$ and $\ell \le \star$). The empty label is a universal supertype ($\ell \le \epsilon$).
* **Intersection ("And"):** * $\frac{\ell_1 \le \ell_3}{\ell_1 \& \ell_2 \le \ell_3} \quad \frac{\ell_2 \le \ell_3}{\ell_1 \& \ell_2 \le \ell_3}$
    * $\frac{\ell_1 \le \ell_2 \quad \ell_1 \le \ell_3}{\ell_1 \le \ell_2 \& \ell_3}$
* **Gradual Union ("Or") - PLAUSIBILITY BASED:** * $\frac{\ell_1 \le \ell_3}{\ell_1 + \ell_2 \le \ell_3} \quad \frac{\ell_2 \le \ell_3}{\ell_1 + \ell_2 \le \ell_3}$
    * $\frac{\ell_3 \le \ell_1}{\ell_3 \le \ell_1 + \ell_2} \quad \frac{\ell_3 \le \ell_2}{\ell_3 \le \ell_1 + \ell_2}$

### 3.2 Simple Types ($\tau_1 \le \tau_2$)
* **Base Reflexivity:** $\iota \le \iota$
* **Bottom & Unknown Types:** $\perp \le \tau$, $\star \le \tau$, $\tau \le \star$
* **Gradual Union Subtyping:** Follows identical plausibility logic to label unions.

### 3.3 Property Types ($R_1 \le R_2$)
* **Bottom Property:** $\perp \le R$
* **Closed Records** (Width subtyping is explicitly forbidden to ensure exact matching):
    $$\frac{\forall i . \tau_i \le \tau'_i}{\{ \overline{a_i : \tau_i}^i \} \le \{ \overline{a_i : \tau'_i}^i \}}$$
* **Open Records** (Width subtyping is allowed, permitting additional fields):
    $$\frac{\{ \overline{a_i : \tau_i}^i \} \le \{ \overline{a_i : \tau'_i}^i \}}{\{ \overline{a_i : \tau_i}^i , \star \} \le \{ \overline{a_i : \tau'_i}^i , \overline{a_j : \tau'_j}^j \}}$$
    $$\frac{\{ \overline{a_i : \tau_i}^i \} \le \{ \overline{a_i : \tau'_i}^i \}}{\{ \overline{a_i : \tau_i}^i , \overline{a_j : \tau_j}^j \} \le \{ \overline{a_i : \tau'_i}^i , \star \}}$$

### 3.4 Descriptor and Variable Types ($T_1 \le T_2$)
* **Descriptor Subtyping:** $\frac{\ell_1 \le \ell_2 \quad R_1 \le R_2}{\ell_1 R_1 \le \ell_2 R_2}$
* **Node/Edge Variables:** * $\frac{L_1 \le L_2}{(| L_1 |) \le (| L_2 |)}$
    * $\frac{L_1 \le L_2 \quad N_1 \le N_3 \quad N_2 \le N_4}{N_1 \xrightarrow{L_1} N_2 \le N_3 \xrightarrow{L_2} N_4}$
* **Unions, Lists, and Null:** * $\frac{T_1 \le T_2}{[T_1]^l \le [T_2]^l}$
    * $\perp \le T$
    * $\text{Null} \le \text{Null}$

---

## 4. The Meet Operator ($\sqcap$)
The meet operation determines the most precise type information when comparing two types. When dealing with unions or non-exact matches, it utilizes a join operator ($\sqcup$) which returns the other element if one is $\perp$, and otherwise returns their gradual union ($+$). Undefined cases return $\perp$.

### 4.1 Label and Simple Types
* **Labels:** $1_1 \sqcap 1_2 = 1_1 \& 1_2$. If either label is $\star$, it yields the other label ($\star \sqcap \ell = \ell$).
* **Simple Types:** $\iota \sqcap \iota = \iota$. Yields to the more precise type when facing unknown: $\star \sqcap \tau = \tau$. For unions: $(\tau_1 + \tau_2) \sqcap \tau = (\tau_1 \sqcap \tau) \sqcup (\tau_2 \sqcap \tau)$.

### 4.2 Property Types ($R_1 \sqcap R_2$)
Pairs common attributes and extends the record when dealing with open property types.
* **Closed / Closed:** $\{\overline{a_i \mapsto \tau_{i1}}^i\} \sqcap \{\overline{a_i \mapsto \tau_{i2}}^i\} = \{\overline{a_i \mapsto \tau_{i1} \sqcap \tau_{i2}}^i\}$
* **Open / Open:** $\{\overline{a_i \mapsto \tau_{i1}}^i, \overline{a_j \mapsto \tau_j}^j, \star\} \sqcap \{\overline{a_i \mapsto \tau_{i2}}^i, \overline{a_k \mapsto \tau_k}^k, \star\} = \{\overline{a_i \mapsto \tau_{i1} \sqcap \tau_{i2}}^i, \overline{a_j \mapsto \tau_j}^j, \overline{a_k \mapsto \tau_k}^k, \star\}$
* **Open / Closed:** $\{\overline{a_i \mapsto \tau_{i1}}^i, \star\} \sqcap \{\overline{a_i \mapsto \tau_{i2}}^i, \overline{a_k \mapsto \tau_k}^k\} = \{\overline{a_i \mapsto \tau_{i1} \sqcap \tau_{i2}}^i, \overline{a_k \mapsto \tau_k}^k\}$

### 4.3 Descriptor and Variable Types
* **Descriptors:** $\ell_1 R_1 \sqcap \ell_2 R_2 = (\ell_1 \sqcap \ell_2) \ (R_1 \sqcap R_2)$
* **Nodes:** $(| L_1 |) \sqcap (| L_2 |) = (| L_1 \sqcap L_2 |)$
* **Directed Edges:** $N_{11} \xrightarrow{L_1} N_{12} \sqcap N_{21} \xrightarrow{L_2} N_{22} = (N_{11} \sqcap N_{21}) \xrightarrow{L_1 \sqcap L_2} (N_{12} \sqcap N_{22})$
* **Undirected Edges** (Accounts for both possible orientations and joins them):
    $$N_{11} \sim^{L_1} N_{12} \sqcap N_{21} \sim^{L_2} N_{22} = T_3 \sqcup T_4$$
    Where $T_3 = (N_{11} \sqcap N_{21}) \sim^{L_1 \sqcap L_2} (N_{12} \sqcap N_{22})$ and $T_4 = (N_{11} \sqcap N_{22}) \sim^{L_1 \sqcap L_2} (N_{12} \sqcap N_{21})$.
* **Lists** (Takes the maximum of the two repetition bounds):
    $$[T_1]^{l_1} \sqcap [T_2]^{l_2} = [T_1 \sqcap T_2]^{\max(l_1, l_2)}$$

---

## 5. Path Operations and Expressions
The calculus must securely handle the concatenation of paths and the typing of expressions (including binary/unary operators and property projections).

### 5.1 Path Concatenation ($\pi_1 \pi_2$)
* **Dynamic Semantics:** Two paths, $p_1$ and $p_2$ concatenate, if the last node of $p_1$ is equal to the first node of $p_2$.
* **Static Typing:** To type-check a concatenation pattern, the path types ($P_1$ and $P_2$) and typing environments ($\Gamma_1$ and $\Gamma_2$) of the subpatterns must be derived recursively. The resulting path type and type environment are computed using their corresponding refinement operators.
    $$\frac{S\vdash\pi_1:P_1;\Gamma_1 \quad S\vdash\pi_2:P_2;\Gamma_2 \quad S\vdash P_1 \sqcap P_2 \triangleright P \quad S\vdash \Gamma_1 \sqcap \Gamma_2 \triangleright \Gamma}{S\vdash\pi_1\pi_2:P;\Gamma}$$

### 5.2 Expressions & Codomain Resolution
Expressions rely heavily on a codomain (`cod`) meta-function to compute resulting types and propagate errors. If the meet operation yields $\perp$ (meaning types are incompatible), the `cod` function returns $\perp$ to propagate the type inconsistency.

* **Binary Operations (bop):** Each subterm is typed and checked against the expected domain type using the meet operator.
    $$\frac{\Delta(\text{bop})=\tau_1 \times \tau_2 \rightarrow \tau_3 \quad \Gamma \vdash t_1:\tau'_1 \quad \Gamma \vdash t_2:\tau'_2}{\Gamma \vdash t_1 \text{ bop } t_2 : \text{cod}(\tau_1 \sqcap \tau'_1 \times \tau_2 \sqcap \tau'_2 \rightarrow \tau_3)}$$
* **Unary Operations (uop):**
    $$\frac{\Delta(\text{uop})=\tau_1 \rightarrow \tau_2 \quad \Gamma \vdash t:\tau'_1}{\Gamma \vdash \text{uop} \ t : \text{cod}(\tau_1 \sqcap \tau'_1 \rightarrow \tau_2)}$$
* **Projections ($x.a$):** Typing is determined by retrieving the type of attribute $a$ from the variable type $\Gamma(x)$. If an attribute $a$ is not found, the return type is $\perp$.

---

## 6. Refinement Operator ($\triangleright$)
The judgment $S \vdash T_1 \triangleright T_2$ extracts possible types for a variable type, a path pattern, a type environment, or a path type. It applies the meet operation to each subtype node or edge in the schema and joins the results.

* **Node Refinement:** $S \vdash N \triangleright \bigsqcup_{N' \le N \in S} N \sqcap N'$
* If no matching type exists in the schema, the operator returns $\perp$, indicating the type is unsatisfiable.
* **Union Refinement:** $\frac{S \vdash T_1 \triangleright T'_1 \quad S \vdash T_2 \triangleright T'_2}{S \vdash T_1 + T_2 \triangleright T'_1 \sqcup T'_2}$

---

## 7. Typing Rules ($S \vdash \pi : P; \Gamma$)
This defines how a parsed pattern $\pi$ is assigned a Path Type $P$ and a Type Environment $\Gamma$ against a Schema $S$.

### 7.1 Nodes and Edges
Node and edge patterns are typed by applying the refinement operator to extract valid matches from the schema.
* **Nodes:** $\frac{S \vdash (x?:L) \triangleright T}{S \vdash (x?:L) : \lfloor T \rfloor^{-} ; x? \mapsto T}$
* **Edges:** $\frac{S \vdash \xleftarrow{x?:L} \triangleright T}{S \vdash \xleftarrow{x?:L} : \lfloor T \rfloor^{\Leftrightarrow} ; x? \mapsto T}$ (Direction defines the resulting path type alignment).

### 7.2 Path Concatenation ($\pi_1 \pi_2$)
To type-check a concatenation pattern, the path types and typing environments of the subpatterns must be derived recursively, then unified using the meet and refinement operators to ensure boundary nodes align.
$$\frac{S\vdash\pi_1:P_1;\Gamma_1 \quad S\vdash\pi_2:P_2;\Gamma_2 \quad S\vdash P_1 \sqcap P_2 \triangleright P \quad S\vdash \Gamma_1 \sqcap \Gamma_2 \triangleright \Gamma}{S\vdash\pi_1\pi_2:P;\Gamma}$$

### 7.3 Union Patterns ($\pi_1 + \pi_2$)
Unlike concatenation (which requires exact consistency), unions use the join ($\sqcup$) operator. For variables present in only one subpattern, the resulting variable type is unioned with `Null`.
$$\frac{S \vdash \pi_1 : P_1 ; \Gamma_1 \quad S \vdash \pi_2 : P_2 ; \Gamma_2}{S \vdash \pi_1 + \pi_2 : P_1 \sqcup P_2 ; (\Gamma_1 \sqcup \Gamma_2)}$$

### 7.4 Expressions and Conditionals
* **Conditionals:** If the conditional expression evaluates to $\perp$ when met with a boolean, the path fails: $\frac{S \vdash \pi : P ; \Gamma \quad \Gamma \vdash t : \tau \sqcap \mathbb{B} = \perp}{S \vdash \pi_{\langle t \rangle} : \perp ; \Gamma}$

---

## 8. Core Meta-Functions
These recursive functions are essential for the typing rules and runtime logic.

### 8.1 Direction Resolution ($\lfloor T \rfloor^{\Leftrightarrow}$)
When converting an Edge Type to a Path Type, the direction dictates the structure:
* **Forward:** $\lfloor N_1 \rightarrow N_2 \rfloor^{\rightarrow} = N_1 - N_2$
* **Backward:** $\lfloor N_1 \rightarrow N_2 \rfloor^{\leftarrow} = N_2 - N_1$
* **Undirected/Any:** $\lfloor N_1 \sim N_2 \rfloor^{\sim} = (N_1 - N_2) + (N_2 - N_1)$
* *Note:* For node types or $\perp$, it returns the type itself.

### 8.2 Path Meet & Refinement ($S \vdash P_1 \sqcap P_2 \triangleright P_3$)
When merging two paths, the meet operation is *strictly* applied to the **last node type of $P_1$** and the **first node type of $P_2$**.
* **Single Nodes:** $\frac{S \vdash L_1 \sqcap L_2 \triangleright L_3}{S \vdash (|L_1|) \sqcap (|L_2|) \triangleright (|L_3|)}$
* **Right Extension:** $\frac{S \vdash N_2 \sqcap N_3 \triangleright N_4}{S \vdash (P_1 - N_2) \sqcap N_3 \triangleright P_1 - N_4}$
* **Left Extension:** $\frac{S \vdash P_1 \sqcap P_2 \triangleright P_3}{S \vdash P_1 \sqcap (P_2 - N_3) \triangleright P_3 - N_3}$

### 8.3 Path Length ($len(P)$)
Used to validate repetition patterns ($\pi^{l..u}$) to prevent infinite loops. It calculates the minimum number of edges:
* $len(\perp) = 0$
* $len(N) = 0$
* $len(N_1 - N_2) = 1$
* $len(P_1 + P_2) = \min(len(P_1), len(P_2))$

### 8.4 The `empty()` Function
Checks if a type environment or path type mathematically guarantees an empty result.
* **Standard Types:** Returns `true` if $\perp$ (error/bottom) is found anywhere.
* **Union Types ($T_1 + T_2$):** Returns `true` **ONLY** if both subcomponents contain $\perp$. If only one contains an error, the union might still yield a non-empty result at runtime, so it returns `false`.
* **Environments:** `empty(\Gamma)` is `true` if $\exists x \in dom(\Gamma)$ where `empty(\Gamma(x))` is `true`.

---

## 9. Precision and Gradual Guarantees ($\sqsubseteq$)
To prove the Static and Dynamic Gradual Guarantees (SGG and DGG), FPPC enforces a precision relation. A type is considered more precise than another if it contains fewer unknown ($\star$) elements. 

* **Simple Types:** $\tau \sqsubseteq \star \quad \tau \sqsubseteq \tau + \tau_2 \quad \perp \sqsubseteq \tau$ (Bottom represents an error state, making it more precise than any other type).
* **Property Types:** $\{\overline{a_i : \tau_i}^i\} \sqsubseteq \{\overline{a_i : \tau_i}^i, \star\}$
* **Labels:** $1 \sqsubseteq \star \quad \ell_1 \sqsubseteq \ell_1 + \ell_2 \quad \ell \sqsubseteq \epsilon$

**The Gradual Guarantee Contract:**
If a query successfully type checks, removing type annotations (making it less precise) will either introduce no static type errors or strictly increase the resulting output set. *Restriction for DGG:* Type tests (`is`) and casts (`as`) must remain invariant in their underlying type argument to prevent monotonicity violations.

---

## 10. Core Theorems
* **Theorem 6.3 (Soundness/Type Safety):** If a pattern successfully type checks ($S \vdash \pi : P; \Gamma$) and the Graph is well-formed against the Schema, the pattern will not get stuck during runtime, and its resulting paths and assignments will map perfectly to the static types.
* **Theorem 6.5 (Emptiness):** If a pattern successfully type checks, but its Path Type ($P$) or Type Environment ($\Gamma$) contains an empty result ($\perp$), the evaluation is mathematically guaranteed to return an empty set.