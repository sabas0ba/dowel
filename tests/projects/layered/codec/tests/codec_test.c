#include <codec/codec.h>

/* base への依存は公開なので、codec を使う側からも推移的に見える。 */
#include <base/base.h>

#ifdef CODEC_INTERNAL
#error "a private define must not reach dependents"
#endif
#ifndef BASE_API
#error "a public define must propagate transitively"
#endif

int main(void)
{
    return (codec_encode(1) == 2 && base_sum(1, 1) == 2) ? 0 : 1;
}
