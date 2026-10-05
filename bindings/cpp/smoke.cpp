#include "rapidpdftrans.hpp"

#include <iostream>
#include <string>

int main(int argc, char** argv) {
  if (argc < 2) {
    std::cerr << "usage: smoke hello.pdf\n";
    return 2;
  }
  try {
    rpt::Document doc(argv[1]);
    const std::string json = doc.extract();
    if (json.find("Hello") == std::string::npos) {
      std::cerr << "extract did not contain Hello\n";
      return 1;
    }
    bool save_failed = false;
    try {
      doc.save("/tmp/rpt-should-not-write.pdf");
    } catch (const std::runtime_error& err) {
      save_failed = std::string(err.what()).find("RPT_LLM_API_KEY") != std::string::npos;
    }
    if (!save_failed) {
      std::cerr << "save without RPT_LLM_API_KEY should fail\n";
      return 1;
    }
  } catch (const std::exception& err) {
    std::cerr << err.what() << "\n";
    return 1;
  }
  return 0;
}
