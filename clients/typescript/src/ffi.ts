/**
 * Private FFI bindings for librebar_ffi. Do not import directly.
 */

const libName = Deno.build.os === "darwin"
  ? "librebar_ffi.dylib"
  : Deno.build.os === "windows"
  ? "rebar_ffi.dll"
  : "librebar_ffi.so";

const libPath = Deno.env.get("REBAR_LIB_PATH") ?? libName;

export const lib = Deno.dlopen(libPath, {
  rebar_msg_create: {
    parameters: ["buffer", "usize"],
    result: "pointer",
  },
  rebar_msg_data: {
    parameters: ["pointer"],
    result: "pointer",
  },
  rebar_msg_len: {
    parameters: ["pointer"],
    result: "usize",
  },
  rebar_msg_free: {
    parameters: ["pointer"],
    result: "void",
  },
  rebar_runtime_new: {
    parameters: ["u64"],
    result: "pointer",
  },
  rebar_runtime_free: {
    parameters: ["pointer"],
    result: "void",
  },
  rebar_spawn: {
    parameters: ["pointer", "function", "buffer"],
    result: "i32",
  },
  rebar_send: {
    parameters: ["pointer", { struct: ["u64", "u64"] }, "pointer"],
    result: "i32",
  },
  rebar_register: {
    parameters: ["pointer", "buffer", "usize", { struct: ["u64", "u64"] }],
    result: "i32",
  },
  rebar_whereis: {
    parameters: ["pointer", "buffer", "usize", "buffer"],
    result: "i32",
  },
  rebar_send_named: {
    parameters: ["pointer", "buffer", "usize", "pointer"],
    result: "i32",
  },
});

// Callback definition for rebar_spawn
export const processCallbackDef = {
  parameters: [{ struct: ["u64", "u64"] }],
  result: "void",
} as const;
