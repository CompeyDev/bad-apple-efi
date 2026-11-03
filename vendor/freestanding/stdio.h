#pragma once
#include <stdarg.h>
#include <stddef.h>

typedef void FILE;

int vsnprintf(char *str, size_t size, const char *format, va_list ap);
