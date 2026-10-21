#include <Windows.h>
#include <dia2.h>
#include <cvconst.h>
#include <iostream>
#include <stdexcept>
#include <string>

template<class T> struct Com {
    T* pointer = nullptr;
    ~Com() { if (pointer) pointer->Release(); }
    T* operator->() const { return pointer; }
    T** receive() { return &pointer; }
    Com() = default;
    Com(const Com&) = delete;
    Com& operator=(const Com&) = delete;
};

static void require(HRESULT result, const char* operation) {
    if (FAILED(result)) {
        std::cerr << operation << " HRESULT=0x" << std::hex << result << std::dec << '\n';
        throw std::runtime_error(operation);
    }
}

static std::wstring name(IDiaSymbol* symbol) {
    BSTR text = nullptr;
    if (symbol->get_name(&text) != S_OK) return L"";
    std::wstring result(text, SysStringLen(text));
    SysFreeString(text);
    return result;
}

static void describe(IDiaSymbol* symbol) {
    DWORD tag = 0;
    ULONGLONG length = 0;
    symbol->get_symTag(&tag);
    symbol->get_length(&length);
    std::wcout << L"tag=" << tag << L" size=" << length << L" name=" << name(symbol);
    DWORD base = 0;
    if (symbol->get_baseType(&base) == S_OK) std::wcout << L" base=" << base;
    LONG offset = 0;
    if (symbol->get_offset(&offset) == S_OK) std::wcout << L" offset=" << offset;
    DWORD kind = 0;
    if (symbol->get_dataKind(&kind) == S_OK) std::wcout << L" data_kind=" << kind;
    Com<IDiaSymbol> type;
    if (symbol->get_type(type.receive()) == S_OK && type.pointer) {
        DWORD type_tag = 0, type_base = 0;
        ULONGLONG type_size = 0;
        type->get_symTag(&type_tag);
        type->get_length(&type_size);
        type->get_baseType(&type_base);
        std::wcout << L" type={tag=" << type_tag << L" size=" << type_size << L" base=" << type_base << L" name=" << name(type.pointer) << L"}";
    }
    VARIANT value;
    VariantInit(&value);
    if (symbol->get_value(&value) == S_OK) {
        std::wcout << L" value_vt=" << value.vt;
        if (value.vt == VT_I4) std::wcout << L" value=" << value.lVal;
        if (value.vt == VT_UI4) std::wcout << L" value=" << value.ulVal;
        if (value.vt == VT_I8) std::wcout << L" value=" << value.llVal;
        if (value.vt == VT_UI8) std::wcout << L" value=" << value.ullVal;
    }
    VariantClear(&value);
    std::wcout << L'\n';
}

static void inspect(IDiaSymbol* global, const wchar_t* query) {
    std::wcout << L"QUERY " << query << L'\n';
    unsigned total = 0;
    for (auto tag : {SymTagUDT, SymTagEnum}) {
        Com<IDiaEnumSymbols> matches;
        require(global->findChildren(tag, query, nsRegularExpression, matches.receive()), "findChildren");
        while (true) {
            Com<IDiaSymbol> symbol;
            ULONG fetched = 0;
            require(matches->Next(1, symbol.receive(), &fetched), "Next symbol");
            if (!fetched) break;
            if (++total > 1000) throw std::runtime_error("symbol match limit exceeded");
            describe(symbol.pointer);
            Com<IDiaEnumSymbols> members;
            require(symbol->findChildren(SymTagNull, nullptr, nsNone, members.receive()), "find members");
            unsigned count = 0;
            while (true) {
                Com<IDiaSymbol> member;
                require(members->Next(1, member.receive(), &fetched), "Next member");
                if (!fetched) break;
                if (++count > 5000) throw std::runtime_error("member limit exceeded");
                std::wcout << L"  ";
                describe(member.pointer);
            }
        }
    }
    std::wcout << L"MATCHES " << total << L'\n';
}

int wmain(int argc, wchar_t** argv) {
    if (argc < 4) {
        std::wcerr << L"usage: pvd-symbol-types <existing-msdia140.dll> <target.pdb> <DIA-name-pattern> [...]\n";
        return 2;
    }
    HMODULE module = LoadLibraryW(argv[1]);
    if (!module) { std::cerr << "LoadLibrary failed: " << GetLastError() << '\n'; return 1; }
    int result = 0;
    try {
        using GetFactory = HRESULT (WINAPI*)(REFCLSID, REFIID, LPVOID*);
        auto get_factory = reinterpret_cast<GetFactory>(GetProcAddress(module, "DllGetClassObject"));
        if (!get_factory) throw std::runtime_error("DllGetClassObject missing");
        Com<IClassFactory> factory;
        require(get_factory(__uuidof(DiaSource), __uuidof(IClassFactory), reinterpret_cast<void**>(factory.receive())), "Get DIA class factory");
        Com<IDiaDataSource> source;
        require(factory->CreateInstance(nullptr, __uuidof(IDiaDataSource), reinterpret_cast<void**>(source.receive())), "Create DIA source");
        require(source->loadDataFromPdb(argv[2]), "Load PDB");
        Com<IDiaSession> session;
        require(source->openSession(session.receive()), "Open session");
        Com<IDiaSymbol> global;
        require(session->get_globalScope(global.receive()), "Global scope");
        describe(global.pointer);
        BOOL stripped = FALSE;
        if (global->get_isStripped(&stripped) == S_OK) std::wcout << L"is_stripped=" << stripped << L'\n';
        GUID guid{};
        DWORD age = 0;
        if (global->get_guid(&guid) == S_OK && global->get_age(&age) == S_OK) {
            wchar_t text[40]{};
            StringFromGUID2(guid, text, 40);
            std::wcout << L"guid=" << text << L" age=" << age << L'\n';
        }
        for (auto tag : {SymTagUDT, SymTagEnum, SymTagFunction, SymTagPublicSymbol}) {
            Com<IDiaEnumSymbols> symbols;
            require(global->findChildren(tag, nullptr, nsNone, symbols.receive()), "Count symbols");
            LONG count = 0;
            require(symbols->get_Count(&count), "Symbol count");
            std::wcout << L"inventory tag=" << tag << L" count=" << count << L'\n';
        }
        for (int index = 3; index < argc; ++index) inspect(global.pointer, argv[index]);
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        result = 1;
    }
    FreeLibrary(module);
    return result;
}
