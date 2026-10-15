cmake_minimum_required(VERSION 3.16)
project(rapidpdftrans_sdk_smoke LANGUAGES CXX)

if(NOT DEFINED RPT_SDK_ROOT)
  message(FATAL_ERROR "Set RPT_SDK_ROOT to the extracted SDK")
endif()

find_package(rapidpdftrans REQUIRED CONFIG PATHS "${RPT_SDK_ROOT}" NO_DEFAULT_PATH)

set(SMOKE_CPP "${CMAKE_CURRENT_LIST_DIR}/../../bindings/cpp/smoke.cpp")

add_executable(smoke_shared "${SMOKE_CPP}")
target_compile_features(smoke_shared PRIVATE cxx_std_17)
target_link_libraries(smoke_shared PRIVATE rapidpdftrans::shared)
if(UNIX AND NOT APPLE)
  set_target_properties(smoke_shared PROPERTIES BUILD_RPATH "$ORIGIN")
elseif(APPLE)
  set_target_properties(smoke_shared PROPERTIES BUILD_RPATH "@loader_path")
endif()
add_custom_command(TARGET smoke_shared POST_BUILD
  COMMAND ${CMAKE_COMMAND} -E copy_if_different
          $<TARGET_FILE:rapidpdftrans::shared>
          $<TARGET_FILE_DIR:smoke_shared>
)

add_executable(smoke_static "${SMOKE_CPP}")
target_compile_features(smoke_static PRIVATE cxx_std_17)
target_link_libraries(smoke_static PRIVATE rapidpdftrans::static)
