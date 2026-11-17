//! La carpeta temporal de los tests (`comun::Temporal`) se borra aunque algo tarde en soltar sus ficheros.
//! Solo en Windows: en los demás sistemas un fichero abierto se borra igual.
#![cfg(windows)]

mod comun;

/// En Windows un fichero abierto sin compartir el borrado no se puede borrar: si otro proceso (git, el
/// antivirus) lo suelta unos segundos tarde, la carpeta se borra igual en cuanto lo suelta. Con 5 s de
/// margen, una batería de `sync` con la máquina cargada dejaba carpetas en `%TEMP%`.
#[test]
fn se_borra_aunque_un_fichero_tarde_en_soltarse() {
    use std::os::windows::fs::OpenOptionsExt;
    let t = comun::Temporal::nueva("temporal-tarde");
    let ruta = t.to_path_buf();
    // Sin `FILE_SHARE_DELETE` (el `File::create` de Rust sí lo comparte y se podría borrar abierto).
    let fichero = std::fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(0).open(ruta.join("abierto")).unwrap();
    let soltar = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(7));
        drop(fichero);
    });
    drop(t);
    soltar.join().unwrap();
    assert!(!ruta.exists(), "quedó {}", ruta.display());
}
