#include <codec/codec.h>

#include <base/base.h>

#ifndef CODEC_INTERNAL
#error "a private define must reach the target's own compilation"
#endif
#ifndef BASE_API
#error "a dependency's public define must reach the compilation"
#endif

int codec_encode(int v)
{
    return base_sum(v, 1);
}
