cmake_minimum_required(VERSION 3.20)
project(pvd_apex_clothing LANGUAGES CXX)

set(PVD_PROJECT_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." CACHE PATH "pvd-studio source directory")
set(PHYSX34_ROOT "" CACHE PATH "Pinned PhysX 3.4 checkout")
set(APEX14_ROOT "" CACHE PATH "Pinned APEX 1.4 staging directory")
if(NOT WIN32 OR NOT MSVC)
  message(FATAL_ERROR "This producer requires Windows and MSVC.")
endif()
if(NOT EXISTS "${PVD_PROJECT_ROOT}/tools/physx-features/feature_harness.h")
  message(FATAL_ERROR "Set PVD_PROJECT_ROOT to the pvd-studio source directory.")
endif()
if(NOT EXISTS "${PHYSX34_ROOT}/PhysX_3.4/Include/PxPhysicsAPI.h")
  message(FATAL_ERROR "Set PHYSX34_ROOT to the pinned PhysX checkout.")
endif()
if(NOT EXISTS "${APEX14_ROOT}/include/ApexSDK.h")
  message(FATAL_ERROR "Set APEX14_ROOT to the verified APEX staging directory.")
endif()

add_executable(pvd-apex-clothing main.cpp)
target_compile_features(pvd-apex-clothing PRIVATE cxx_std_14)
set_property(TARGET pvd-apex-clothing PROPERTY MSVC_RUNTIME_LIBRARY MultiThreaded)
target_compile_definitions(pvd-apex-clothing PRIVATE NDEBUG PX_CHECKED=1)
target_include_directories(pvd-apex-clothing PRIVATE
  "${PVD_PROJECT_ROOT}/tools/physx-features"
  "${PVD_PROJECT_ROOT}/tools/physx-sample/compat"
  "${PHYSX34_ROOT}/PhysX_3.4/Include"
  "${PHYSX34_ROOT}/PxShared/include"
  "${PHYSX34_ROOT}/PxShared/src/foundation/include"
  "${PHYSX34_ROOT}/PxShared/src/pvd/include"
  "${APEX14_ROOT}/include"
  "${APEX14_ROOT}/include/PhysX3"
  "${APEX14_ROOT}/shared/general/RenderDebug/public")
target_link_libraries(pvd-apex-clothing PRIVATE
  "${APEX14_ROOT}/lib/vc15win64-PhysX_3.4/ApexFrameworkCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CommonCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CookingCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3ExtensionsCHECKED.lib"
  "${PHYSX34_ROOT}/PxShared/lib/vc15win64/PxFoundationCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PxShared/lib/vc15win64/PxPvdSDKCHECKED_x64.lib")
foreach(runtime
    PhysX_3.4/Bin/vc15win64/PhysX3CHECKED_x64.dll
    PhysX_3.4/Bin/vc15win64/PhysX3CommonCHECKED_x64.dll
    PhysX_3.4/Bin/vc15win64/PhysX3CookingCHECKED_x64.dll
    PxShared/bin/vc15win64/PxFoundationCHECKED_x64.dll
    PxShared/bin/vc15win64/PxPvdSDKCHECKED_x64.dll
    Externals/nvToolsExt/1/bin/x64/nvToolsExt64_1.dll)
  add_custom_command(TARGET pvd-apex-clothing POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different "${PHYSX34_ROOT}/${runtime}" "$<TARGET_FILE_DIR:pvd-apex-clothing>")
endforeach()
foreach(runtime ApexFrameworkCHECKED_x64.dll APEX_ClothingCHECKED_x64.dll APEX_LegacyCHECKED_x64.dll)
  add_custom_command(TARGET pvd-apex-clothing POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different "${APEX14_ROOT}/bin/vc15win64-PhysX_3.4/${runtime}" "$<TARGET_FILE_DIR:pvd-apex-clothing>")
endforeach()
