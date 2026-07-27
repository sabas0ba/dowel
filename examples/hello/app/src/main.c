#include <stdio.h>

#include "greet.h"

int main(void) {
    printf("%s (opt=%d api=%d)\n", greet_message(), APP_OPT, GREET_API);
    return 0;
}
