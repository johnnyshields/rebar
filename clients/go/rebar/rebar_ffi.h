/* rebar_ffi.h — C type declarations for rebar-ffi Go bindings */
#ifndef REBAR_FFI_H
#define REBAR_FFI_H

#include <stdint.h>
#include <stddef.h>

/* Types */
typedef struct {
    uint64_t node_id;
    uint64_t local_id;
} rebar_pid_t;

typedef struct rebar_msg_t rebar_msg_t;
typedef struct rebar_runtime_t rebar_runtime_t;

/* Error codes */
#define REBAR_OK              0
#define REBAR_ERR_NULL_PTR   -1
#define REBAR_ERR_SEND_FAILED -2
#define REBAR_ERR_NOT_FOUND  -3
#define REBAR_ERR_INVALID_NAME -4

/* Message API */
rebar_msg_t *rebar_msg_create(const uint8_t *data, size_t len);
const uint8_t *rebar_msg_data(const rebar_msg_t *msg);
size_t rebar_msg_len(const rebar_msg_t *msg);
void rebar_msg_free(rebar_msg_t *msg);

/* Runtime API */
rebar_runtime_t *rebar_runtime_new(uint64_t node_id);
void rebar_runtime_free(rebar_runtime_t *rt);

/* Process API */
typedef void (*rebar_process_fn)(rebar_pid_t);
int32_t rebar_spawn(rebar_runtime_t *rt, rebar_process_fn callback,
                    rebar_pid_t *pid_out);
int32_t rebar_send(rebar_runtime_t *rt, rebar_pid_t dest,
                   const rebar_msg_t *msg);

/* Registry API */
int32_t rebar_register(rebar_runtime_t *rt, const uint8_t *name,
                       size_t name_len, rebar_pid_t pid);
int32_t rebar_whereis(rebar_runtime_t *rt, const uint8_t *name,
                      size_t name_len, rebar_pid_t *pid_out);
int32_t rebar_send_named(rebar_runtime_t *rt, const uint8_t *name,
                         size_t name_len, const rebar_msg_t *msg);

#endif /* REBAR_FFI_H */
