package rapidpdftrans

import (
	"strings"
	"testing"
)

func TestExtractHello(t *testing.T) {
	text, err := Extract("../../testdata/hello.pdf")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(text, "Hello") {
		t.Fatalf("extract missing Hello: %s", text)
	}
	if !strings.Contains(text, "pending") {
		t.Fatalf("coverage should start pending: %s", text)
	}
}
