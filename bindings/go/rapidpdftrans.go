// Package rapidpdftrans wraps the RapidPdfTrans C ABI.
//
// Build the static library first:
//
//	cargo build -p rpt-ffi --release
package rapidpdftrans

/*
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -L${SRCDIR}/../../target/release -l:librapidpdftrans.a -lm -ldl -lpthread -lgcc_s
#include "rapidpdftrans.h"
#include <stdlib.h>
*/
import "C"
import (
	"errors"
	"unsafe"
)

// Extract returns per-glyph JSON for the PDF at path.
func Extract(path string) (string, error) {
	return ExtractOptions(path, "")
}

// ExtractOptions passes an options JSON object through to the C ABI.
func ExtractOptions(path, options string) (string, error) {
	doc, err := open(path, options)
	if err != nil {
		return "", err
	}
	defer C.rpt_free(doc)
	return callString(func(opts *C.char) *C.char {
		return C.rpt_extract(doc, opts)
	}, options)
}

// Translate extracts and translates. RPT_LLM_API_KEY must be set in the environment.
// The PDF is not rewritten.
func Translate(path, options string) (string, error) {
	doc, err := open(path, options)
	if err != nil {
		return "", err
	}
	defer C.rpt_free(doc)
	return callString(func(opts *C.char) *C.char {
		return C.rpt_translate(doc, opts)
	}, options)
}

func open(path, options string) (*C.RptDocument, error) {
	cpath := C.CString(path)
	defer C.free(unsafe.Pointer(cpath))
	copts, freeOpts := cStringOrNil(options)
	if freeOpts != nil {
		defer freeOpts()
	}
	doc := C.rpt_open(cpath, copts)
	if doc == nil {
		return nil, lastError()
	}
	return doc, nil
}

func callString(call func(*C.char) *C.char, options string) (string, error) {
	copts, freeOpts := cStringOrNil(options)
	if freeOpts != nil {
		defer freeOpts()
	}
	out := call(copts)
	if out == nil {
		return "", lastError()
	}
	defer C.rpt_string_free(out)
	return C.GoString(out), nil
}

func cStringOrNil(value string) (*C.char, func()) {
	if value == "" {
		return nil, nil
	}
	cstr := C.CString(value)
	return cstr, func() { C.free(unsafe.Pointer(cstr)) }
}

func lastError() error {
	msg := C.rpt_last_error()
	if msg == nil {
		return errors.New("rapidpdftrans: unknown error")
	}
	return errors.New(C.GoString(msg))
}
