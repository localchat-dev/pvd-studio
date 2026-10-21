cmake_minimum_required(VERSION 3.20)
project(pvd_physx41_sample LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
  message(FATAL_ERROR "This producer build requires Windows and MSVC.")
endif()
set(PHYSX41_ROOT "" CACHE PATH "Pinned official PhysX 4.1 checkout")
set(PHYSX41_LIB "" CACHE PATH "Checked static PhysX 4.1 library directory")
if(NOT EXISTS "${PHYSX41_ROOT}/physx/include/PxPhysicsAPI.h")
  message(FATAL_ERROR "Set PHYSX41_ROOT to the pinned official PhysX 4.1 checkout.")
endif()
add_executable(pvd-physx41-sample ../physx-sample/main.cpp)
target_compile_features(pvd-physx41-sample PRIVATE cxx_std_14)
set_property(TARGET pvd-physx41-sample PROPERTY MSVC_RUNTIME_LIBRARY MultiThreaded)
target_compile_definitions(pvd-physx41-sample PRIVATE NDEBUG PX_CHECKED=1 PX_PHYSX_STATIC_LIB PX_FOUNDATION_VERSION=PX_PHYSICS_VERSION)
target_include_directories(pvd-physx41-sample PRIVATE "${PHYSX41_ROOT}/physx/include" "${PHYSX41_ROOT}/pxshared/include")
target_link_libraries(pvd-physx41-sample PRIVATE
  "${PHYSX41_LIB}/PhysX_static.lib"
  "${PHYSX41_LIB}/PhysXCommon_static.lib"
  "${PHYSX41_LIB}/PhysXCooking_static.lib"
  "${PHYSX41_LIB}/PhysXExtensions_static.lib"
  "${PHYSX41_LIB}/PhysXFoundation_static.lib"
  "${PHYSX41_LIB}/PhysXPvdSDK_static.lib"
  ws2_32)
