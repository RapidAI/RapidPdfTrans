#pragma once

// Header-only RAII wrapper over include/rapidpdftrans.h.
// PDF rewriting is not implemented; save() throws.

#include <stdexcept>
#include <string>
#include <utility>

#include "rapidpdftrans.h"

namespace rpt {

class Document {
 public:
  explicit Document(const std::string& path, const std::string& options = "") {
    doc_ = rpt_open(path.c_str(), options.c_str());
    if (doc_ == nullptr) {
      throw std::runtime_error(last_error());
    }
  }

  ~Document() {
    if (doc_ != nullptr) {
      rpt_free(doc_);
    }
  }

  Document(const Document&) = delete;
  Document& operator=(const Document&) = delete;

  Document(Document&& other) noexcept : doc_(other.doc_) { other.doc_ = nullptr; }

  Document& operator=(Document&& other) noexcept {
    if (this != &other) {
      if (doc_ != nullptr) {
        rpt_free(doc_);
      }
      doc_ = other.doc_;
      other.doc_ = nullptr;
    }
    return *this;
  }

  std::string extract(const std::string& options = "") const {
    return take(rpt_extract(doc_, options.c_str()));
  }

  std::string translate(const std::string& options = "") const {
    return take(rpt_translate(doc_, options.c_str()));
  }

  void save(const std::string& path, const std::string& options = "") const {
    if (rpt_save(doc_, path.c_str(), options.c_str()) != 0) {
      throw std::runtime_error(last_error());
    }
  }

 private:
  RptDocument* doc_ = nullptr;

  static std::string last_error() {
    const char* message = rpt_last_error();
    if (message == nullptr) {
      return "rapidpdftrans: unknown error";
    }
    return message;
  }

  static std::string take(char* text) {
    if (text == nullptr) {
      throw std::runtime_error(last_error());
    }
    std::string out(text);
    rpt_string_free(text);
    return out;
  }
};

}  // namespace rpt
