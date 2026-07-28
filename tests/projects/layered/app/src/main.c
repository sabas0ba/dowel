#include <stdio.h>

#include <base/base.h>
#include <codec/codec.h>
#include <net/net.h>

int main(void)
{
    printf("sum=%d encode=%d port=%d opt=%d\n",
           base_sum(2, 3), codec_encode(4), net_port(80), APP_OPT);
    return 0;
}
