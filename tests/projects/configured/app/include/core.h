#pragma once

/*
 * 構成に対する主張はここに置く。実行するまでもなく決まる項目であり、
 * コンパイルが通るかどうかで結果が出る。
 */

/* `fast` は `[features]` で `simd` を有効にする。連鎖が解けていなければ落ちる。 */
#if defined(APP_FAST) && !defined(APP_SIMD)
#error "the feature `fast` did not pull in `simd`"
#endif

/* `match cfg.opt` は debug と release の双方にアームを持つ。 */
#ifndef APP_OPT
#error "`match cfg.opt` produced no value"
#endif

#ifdef APP_FAST
#define CORE_FAST 1
#else
#define CORE_FAST 0
#endif

#ifdef APP_SIMD
#define CORE_SIMD 1
#else
#define CORE_SIMD 0
#endif

#ifdef APP_TRACE
#define CORE_TRACE 1
#else
#define CORE_TRACE 0
#endif

#ifdef APP_JSON
#define CORE_JSON 1
#else
#define CORE_JSON 0
#endif

/* 有効な構成を1行で表す。実行して突き合わせる。 */
const char *core_config(void);
