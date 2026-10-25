#pragma once
#include <stddef.h>

void *malloc(size_t size);
void free(void *ptr);
void *calloc(size_t num, size_t size);
void *realloc(void *ptr, size_t size);
void qsort(void *base, size_t nmemb, size_t size,
           int (*compar)(const void *, const void *));
char *getenv(const char *name);
long strtol(const char *str, char **endptr, int base);
unsigned long strtoul(const char *str, char **endptr, int base);

#define abs __builtin_abs
