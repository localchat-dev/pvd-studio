cmake_minimum_required(VERSION 3.20)
project(pvd_repx_sleeping LANGUAGES CXX)

set(PVD_STUDIO_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." CACHE PATH "pvd-studio source directory")
set(PHYSX34_ROOT "" CACHE PATH "Pinned official PhysX 3.4 checkout")
if(NOT EXISTS "${PVD_STUDIO_ROOT}/tools/physx-features/feature_harness.h")
  message(FATAL_ERROR "Set PVD_STUDIO_ROOT to the pvd-studio source directory.")
endif()
add_subdirectory("${PVD_STUDIO_ROOT}/tools/physx-features" "feature-harness" EXCLUDE_FROM_ALL)
add_feature_producer(pvd-repx-sleeping main.cpp)
target_include_directories(pvd-repx-sleeping PRIVATE
  "${PVD_STUDIO_ROOT}/tools/physx-features"
  "${PVD_STUDIO_ROOT}/tools/physx-sample")
