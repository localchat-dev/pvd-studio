cmake_minimum_required(VERSION 3.20)
project(pvd_physx_multiscene LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
  message(FATAL_ERROR "This producer requires the existing Windows MSVC toolchain.")
endif()

set(PHYSX34_ROOT "" CACHE PATH "Pinned official PhysX 3.4 checkout")
if(NOT EXISTS "${PHYSX34_ROOT}/PhysX_3.4/Include/PxPhysicsAPI.h")
  message(FATAL_ERROR "Set PHYSX34_ROOT to the built official PhysX 3.4 checkout.")
endif()

add_executable(pvd-physx-multiscene main.cpp)
target_compile_features(pvd-physx-multiscene PRIVATE cxx_std_14)
set_property(TARGET pvd-physx-multiscene PROPERTY MSVC_RUNTIME_LIBRARY MultiThreaded)
target_compile_definitions(pvd-physx-multiscene PRIVATE NDEBUG PX_CHECKED=1)
target_include_directories(pvd-physx-multiscene PRIVATE
  "${PHYSX34_ROOT}/PhysX_3.4/Include"
  "${PHYSX34_ROOT}/PxShared/include")
target_link_libraries(pvd-physx-multiscene PRIVATE
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CommonCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3ExtensionsCHECKED.lib"
  "${PHYSX34_ROOT}/PxShared/lib/vc15win64/PxFoundationCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PxShared/lib/vc15win64/PxPvdSDKCHECKED_x64.lib")

foreach(runtime
    PhysX_3.4/Bin/vc15win64/PhysX3CHECKED_x64.dll
    PhysX_3.4/Bin/vc15win64/PhysX3CommonCHECKED_x64.dll
    PxShared/bin/vc15win64/PxFoundationCHECKED_x64.dll
    PxShared/bin/vc15win64/PxPvdSDKCHECKED_x64.dll
    Externals/nvToolsExt/1/bin/x64/nvToolsExt64_1.dll)
  add_custom_command(TARGET pvd-physx-multiscene POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
      "${PHYSX34_ROOT}/${runtime}" "$<TARGET_FILE_DIR:pvd-physx-multiscene>")
endforeach()
