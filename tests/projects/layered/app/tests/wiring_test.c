/*
 * 依存の伝播が期待どおりであることを、利用者と同じ立場から確かめる。
 * 見えるべきものは include できて、見えてはならないものは define されていない。
 */

#include <base/base.h>
#include <codec/codec.h>
#include <net/net.h>

/* 2経路（codec と net）から同じ公開定義が届く。衝突扱いされてはならない。 */
#ifndef BASE_API
#error "a transitive public define must reach the dependent"
#endif
#ifndef CODEC_API
#error "a direct dependency's public define must reach the dependent"
#endif
#ifndef NET_API
#error "a direct dependency's public define must reach the dependent"
#endif

/* 非公開のものは、定義もヘッダも届いてはならない。 */
#ifdef CODEC_INTERNAL
#error "a private define must not propagate"
#endif
#ifdef NET_INTERNAL
#error "a private define must not propagate"
#endif
#ifdef UTIL_API
#error "a package reached only through a private dependency must not propagate"
#endif

int main(void)
{
    return (base_sum(2, 3) == 5 && codec_encode(4) == 5 && net_port(80) == 1024) ? 0 : 1;
}
