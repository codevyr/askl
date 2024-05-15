int foo(int a, char b);

int foo(int a, char b) {
    return a + b;
}

int main(int argc, char **argv) {
    return argc + foo(argc, 4);
}