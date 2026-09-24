/* @ts-self-types="./frogql_wasm.d.ts" */

/**
 * A live graph plus the caches that keep query latency flat.
 */
export class Connection {
    static __wrap(ptr) {
        const obj = Object.create(Connection.prototype);
        obj.__wbg_ptr = ptr;
        ConnectionFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        ConnectionFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_connection_free(ptr, 0);
    }
    /**
     * `{ node_labels, edge_labels, node_count, edge_count }`, mirroring
     * the Python/Node `schema()` summary.
     * What the typechecker knows about a query, without running it.
     *
     * This is the half of froGQL a REPL shows and a bare "0 rows" hides.
     * `(n:Calle)-[e]->(m)` against a schema where every `EN_CALLE` points
     * *into* `Calle` is not an empty answer, it is a **provably** empty
     * one — the checker settles it before the runtime is asked, and
     * saying "0 filas" instead sends the reader looking for missing data
     * that was never missing.
     *
     * ```json
     * { "ok": true, "empty": true, "errors": [], "warnings": [...],
     *   "vars": [{ "name": "n", "type": "(:Calle {...})" }] }
     * ```
     *
     * The pipeline is run here rather than through
     * `compile_query_with_diagnostics_with` because that returns the
     * compiled query and drops the `TypeEnvironment` — and the
     * environment is the interesting part: it is what the checker
     * *inferred*, per variable, which no amount of reading the query
     * back tells you.
     *
     * Cheap enough to run on every keystroke: parse, elaborate and check,
     * with no optimizer pass and no graph access at all.
     * @param {string} query
     * @returns {any}
     */
    check(query) {
        const ptr0 = passStringToWasm0(query, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.connection_check(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * @returns {number}
     */
    get edge_count() {
        const ret = wasm.connection_edge_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Execute one GQL statement. Read queries return an array of row
     * objects; data-modifying statements return a counters object.
     * `limit` caps the number of rows (default 100 when omitted on the
     * JS side).
     * @param {string} query
     * @param {number | null} [limit]
     * @returns {any}
     */
    execute(query, limit) {
        const ptr0 = passStringToWasm0(query, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.connection_execute(this.__wbg_ptr, ptr0, len0, isLikeNone(limit) ? Number.MAX_SAFE_INTEGER : (limit) >>> 0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * The active GRAPH TYPE, rendered the way `SHOW GRAPH TYPE DEFAULT`
     * renders it in the REPL: one line per node type and one per edge
     * type, with the properties each carries.
     *
     * `schema()` answers "which labels exist", which is what a sidebar
     * needs and all it needs. This answers "what is in them" — the
     * question anyone writing a query against an unfamiliar database
     * actually has, and the one that was unanswerable in the browser
     * because the formatter was never exposed.
     *
     * On a `.gdb` this reads the catalog; on a JSON graph it is inferred
     * from the data, which is the same split `active_schema` makes.
     * @returns {string}
     */
    graph_type() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.connection_graph_type(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * The active GRAPH TYPE as data, so it can be **drawn**.
     *
     * `graph_type()` renders the same thing as text, which is what the
     * REPL shows and what a reader skims. A schema is a graph, though —
     * node types joined by edge types — and the shape of it is the part
     * a text listing makes you reconstruct in your head. This returns
     * the pieces a diagram needs and lets the page draw them.
     *
     * ```json
     * { "nodes": [{ "name": "fpl", "labels": ["Fpl"],
     *               "props": [{ "key": "fplId", "type": "STRING" }] }],
     *   "edges": [{ "label": "SALE_DE", "from": "fpl", "to": "aerodromo",
     *               "directed": true, "props": [] }] }
     * ```
     *
     * Endpoints are the *names* `typing::format::NodeTypeNames` derives,
     * so the diagram and the text call a type the same thing. An edge
     * whose endpoint matches no declared node type gets `null` there
     * rather than a name that would misdescribe it — the same care
     * `format_schema` takes when it declines to borrow a name.
     * @returns {any}
     */
    graph_type_json() {
        const ret = wasm.connection_graph_type_json(this.__wbg_ptr);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * @returns {number}
     */
    get node_count() {
        const ret = wasm.connection_node_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @returns {any}
     */
    schema() {
        const ret = wasm.connection_schema(this.__wbg_ptr);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Serialise the live merged view (base + overlay) to a JSON string —
     * the unit to hand IndexedDB for persistence. Re-open it later with
     * `open_json`.
     * @returns {string}
     */
    to_json() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.connection_to_json(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
}
if (Symbol.dispose) Connection.prototype[Symbol.dispose] = Connection.prototype.free;

/**
 * Open a `.gdb` image fetched over the network, with its `.ltj` sidecar
 * when the caller has it.
 *
 * ```js
 * const [gdb, ltj] = await Promise.all([
 *   fetch("/santiago.gdb").then(r => r.arrayBuffer()),
 *   fetch("/santiago.gdb.ltj").then(r => r.arrayBuffer()),
 * ]);
 * const conn = open_bytes(new Uint8Array(gdb), new Uint8Array(ltj));
 * ```
 *
 * `ltj` is optional and is the reason to prefer this over `open_json`
 * for anything large: without it the six LTJ trie orderings are rebuilt
 * at open, which is `O(E log E)`. A sidecar that does not describe this
 * database is **refused, not trusted** — the same `(graph_id,
 * node_count, edge_count)` fingerprint a file-backed open checks — and
 * the index is rebuilt instead, so a mismatched pair costs time and
 * never correctness.
 *
 * The connection is read-mostly: DML works, through the same overlay as
 * every other backend, but there is nowhere to write pages back to, so
 * the durable copy is whatever the server serves. `to_json()` still
 * gives a snapshot of the merged view.
 * @param {Uint8Array} gdb
 * @param {Uint8Array | null} [ltj]
 * @returns {Connection}
 */
export function open_bytes(gdb, ltj) {
    const ptr0 = passArray8ToWasm0(gdb, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(ltj) ? 0 : passArray8ToWasm0(ltj, wasm.__wbindgen_malloc);
    var len1 = WASM_VECTOR_LEN;
    const ret = wasm.open_bytes(ptr0, len0, ptr1, len1);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return Connection.__wrap(ret[0]);
}

/**
 * Parse a JSON graph document (`{"nodes": [...], "edges": [...]}`) and
 * open a connection over it. Warms the LTJ index eagerly so the first
 * query is as fast as the rest.
 * @param {string} json
 * @returns {Connection}
 */
export function open_json(json) {
    const ptr0 = passStringToWasm0(json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.open_json(ptr0, len0);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return Connection.__wrap(ret[0]);
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_ef53bc310eb298a0: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_String_8564e559799eccda: function(arg0, arg1) {
            const ret = String(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_string_c236cabd84a4d769: function(arg0) {
            const ret = typeof(arg0) === 'string';
            return ret;
        },
        __wbg___wbindgen_throw_1506f2235d1bdba0: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_error_a6fa202b58aa1cd3: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_new_622fc80556be2e26: function() {
            const ret = new Map();
            return ret;
        },
        __wbg_new_ce1ab61c1c2b300d: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_d90091b82fdf5b91: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_now_190933fa139cc119: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_set_52b1e1eb5bed906a: function(arg0, arg1, arg2) {
            const ret = arg0.set(arg1, arg2);
            return ret;
        },
        __wbg_set_6be42768c690e380: function(arg0, arg1, arg2) {
            arg0[arg1] = arg2;
        },
        __wbg_set_dca99999bba88a9a: function(arg0, arg1, arg2) {
            arg0[arg1 >>> 0] = arg2;
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbindgen_cast_0000000000000001: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0) {
            // Cast intrinsic for `I64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0) {
            // Cast intrinsic for `U64 -> Externref`.
            const ret = BigInt.asUintN(64, arg0);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./frogql_wasm_bg.js": import0,
    };
}

const ConnectionFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_connection_free(ptr, 1));

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('frogql_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
