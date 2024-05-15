int foo(int a, char b);

int foo(int a, char b) {
    return a + b;
}

static int bar(int a) {
    return a + 2;
}

int main(int argc, char **argv) {
    return argc + foo(tar(1, 2), bar(2));
}

int tar();

int tar() {
    return 3;
}