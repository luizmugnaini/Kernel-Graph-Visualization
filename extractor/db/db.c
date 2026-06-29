#include "db.h"
#include "../utils.h"
#include <sqlite3.h>
#include <string.h>

static sqlite3 *db = NULL;
static sqlite3_stmt *stmt_insert_file = NULL;
static sqlite3_stmt *stmt_insert_edge = NULL;

void db_init(const char *db_name) {
    if (sqlite3_open(db_name, &db)) {
        PANIC("Falha ao abrir DB: %s", sqlite3_errmsg(db));
    }

    const char *sql_tables = 
        "CREATE TABLE IF NOT EXISTS Files ("
        "id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT UNIQUE);"
        "CREATE TABLE IF NOT EXISTS Edges ("
        "source_id INTEGER, target_id INTEGER, "
        "FOREIGN KEY(source_id) REFERENCES Files(id), "
        "FOREIGN KEY(target_id) REFERENCES Files(id), "
        "UNIQUE(source_id, target_id));";

    sqlite3_exec(db, sql_tables, 0, 0, 0);

    sqlite3_prepare_v2(db, "INSERT OR IGNORE INTO Files (path) VALUES (?);", -1, &stmt_insert_file, NULL);
    sqlite3_prepare_v2(db, "INSERT OR IGNORE INTO Edges (source_id, target_id) VALUES (?, ?);", -1, &stmt_insert_edge, NULL);
    
    sqlite3_exec(db, "BEGIN TRANSACTION;", 0, 0, 0);
    LOG_INFO("Banco de dados '%s' inicializado com sucesso.", db_name);
}

int db_get_or_create_file_id(const char *filepath) {
    sqlite3_bind_text(stmt_insert_file, 1, filepath, -1, SQLITE_TRANSIENT);
    sqlite3_step(stmt_insert_file);
    sqlite3_reset(stmt_insert_file);

    sqlite3_stmt *stmt_select;
    sqlite3_prepare_v2(db, "SELECT id FROM Files WHERE path = ?;", -1, &stmt_select, NULL);
    sqlite3_bind_text(stmt_select, 1, filepath, -1, SQLITE_TRANSIENT);
    
    int id = -1;
    if (sqlite3_step(stmt_select) == SQLITE_ROW) {
        id = sqlite3_column_int(stmt_select, 0);
    }
    sqlite3_finalize(stmt_select);
    return id;
}

void db_insert_edge(int source_id, int target_id) {
    if (source_id == target_id) return; // Ignore self-includes (name-resolution artifacts).
    sqlite3_bind_int(stmt_insert_edge, 1, source_id);
    sqlite3_bind_int(stmt_insert_edge, 2, target_id);
    sqlite3_step(stmt_insert_edge);
    sqlite3_reset(stmt_insert_edge);
}

void db_close(void) {
    sqlite3_exec(db, "COMMIT TRANSACTION;", 0, 0, 0);
    if (stmt_insert_file) sqlite3_finalize(stmt_insert_file);
    if (stmt_insert_edge) sqlite3_finalize(stmt_insert_edge);
    if (db) sqlite3_close(db);
    LOG_INFO("Transação comitada e banco fechado.");
}

int db_resolve_include_fallback(const char *filename) {
    sqlite3_stmt *stmt;
    const char *sql = "SELECT id FROM Files WHERE path LIKE '%/' || ? OR path = ? LIMIT 1;";
    int id = -1;

    if (sqlite3_prepare_v2(db, sql, -1, &stmt, NULL) == SQLITE_OK) {
        sqlite3_bind_text(stmt, 1, filename, -1, SQLITE_STATIC);
        sqlite3_bind_text(stmt, 2, filename, -1, SQLITE_STATIC);
        
        if (sqlite3_step(stmt) == SQLITE_ROW) {
            id = sqlite3_column_int(stmt, 0);
        }
        sqlite3_finalize(stmt);
    }
    return id;
}
